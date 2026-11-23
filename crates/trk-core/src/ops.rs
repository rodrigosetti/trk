//! Issue operations. Each runs inside a write [`Ctx`]. State changes happen
//! only here:
//!
//! ```text
//! open    → claimed            claim
//! claimed → open               unclaim, reap, lease expiry
//! claimed → closed | waiting   close, wait
//! waiting → closed | claimed | open  check passes | wakes session | reopens
//! open    → closed             close
//! closed  → open               reopen
//! ```

use rand::RngExt;
use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};

use crate::error::{Code, Error, Result, bad_args, not_found};
use crate::model::*;
use crate::store::{Ctx, ISSUE_COLS, row_to_issue};

const ID_ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// How much of a failed check's output is kept in the note.
pub const CHECK_OUTPUT_TAIL: usize = 4096;

#[derive(Debug, Clone, Default)]
pub struct NewIssue {
    /// A fixed id (imports); otherwise one is allocated.
    pub id: Option<String>,
    pub title: String,
    pub description: String,
    pub priority: Option<String>,
    pub issue_type: Option<String>,
    /// Issues that block the new one.
    pub blocked_by: Vec<String>,
    /// Issues the new one blocks.
    pub blocks: Vec<String>,
    /// The issue this one was discovered from. Agents must own its active or waiting session.
    pub followup_of: Option<String>,
}

/// Changes to the authored fields; `None` leaves a field alone.
#[derive(Debug, Clone, Default)]
pub struct Edit {
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub issue_type: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ClaimOpts {
    pub lease: Option<jiff::SignedDuration>,
    pub worktree: Option<String>,
    pub host: Option<String>,
    pub agent: Option<String>,
    pub terminal: Option<String>,
    pub steal: bool,
    pub on_behalf_of: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct WaitOpts {
    /// Shell command run by `trk check`. `None` waits on a human.
    pub check: Option<String>,
    pub interval: Option<jiff::SignedDuration>,
    pub reason: Option<String>,
    pub force: bool,
}

/// What one run of a waiting check produced.
#[derive(Debug, Clone, Default)]
pub struct CheckRun {
    /// `None` when the command was killed or could not start.
    pub exit: Option<i32>,
    pub timed_out: bool,
    /// Combined stdout and stderr.
    pub output: String,
}

/// What recording a check did to the issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckResult {
    /// Exit 0: closed as done.
    Passed,
    /// Exit 75: still waiting.
    Retry,
    /// Anything else: back to open with the output as a note.
    Failed,
    /// A failed check restored its waiting session's claim.
    Woken { claim: i64 },
    /// The issue changed while the check ran; nothing was recorded.
    Skipped,
}

impl CheckResult {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckResult::Passed => "passed",
            CheckResult::Retry => "retry",
            CheckResult::Failed => "failed",
            CheckResult::Woken { .. } => "woken",
            CheckResult::Skipped => "skipped",
        }
    }
}

fn check_priority(p: &str) -> Result<String> {
    let up = p.trim().to_ascii_uppercase();
    if PRIORITIES.contains(&up.as_str()) {
        Ok(up)
    } else {
        Err(
            Error::new(Code::InvalidField, format!("invalid priority '{p}'"))
                .with_detail("allowed", json!(PRIORITIES)),
        )
    }
}

fn check_type(issue_type: &str) -> Result<&str> {
    let issue_type = issue_type.trim();
    if issue_type.is_empty() {
        Err(Error::new(Code::InvalidField, "the type cannot be empty"))
    } else {
        Ok(issue_type)
    }
}

fn tail(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

impl Ctx<'_> {
    fn save_issue(&self, i: &Issue) -> Result<()> {
        self.conn.execute(
            "UPDATE issue SET state = ?2, title = ?3, description = ?4, priority = ?5, type = ?6,
                    close_reason = ?7, waiting_reason = ?8, waiting_check = ?9, check_interval = ?10,
                    next_check_at = ?11, last_check_at = ?12, last_check_exit = ?13, wait_cwd = ?14,
                    closed_at = ?15, updated_at = ?16, wake_count = ?17
             WHERE id = ?1",
            params![
                i.id,
                i.state.as_str(),
                i.title,
                i.description,
                i.priority,
                i.issue_type,
                i.close_reason,
                i.waiting_reason,
                i.waiting_check,
                i.check_interval,
                i.next_check_at,
                i.last_check_at,
                i.last_check_exit,
                i.wait_cwd,
                i.closed_at,
                self.ts(),
                i.wake_count,
            ],
        )?;
        Ok(())
    }

    fn clear_waiting(issue: &mut Issue) {
        issue.waiting_reason = None;
        issue.waiting_check = None;
        issue.check_interval = None;
        issue.next_check_at = None;
        issue.last_check_at = None;
        issue.last_check_exit = None;
        issue.wait_cwd = None;
    }

    fn invalid_state(&self, issue: &Issue, what: &str) -> Error {
        Error::new(
            Code::InvalidState,
            format!("{} is {}; {what}", issue.id, issue.state),
        )
        .with_issue(&issue.id)
        .with_detail("state", issue.state.as_str())
    }

    /// Only the claimant may act on a claimed issue; a human may force it.
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

    fn is_claimed_by_me(&self, id: &str) -> Result<bool> {
        Ok(self
            .active_claim(id)?
            .is_some_and(|c| c.actor == self.actor_str()))
    }

    /// Require the supplied claim or session to belong to the acting agent.
    fn agent_gate(&self, issue_id: &str, claim: &Option<Claim>) -> Result<()> {
        if self.actor().is_agent() && claim.as_ref().is_none_or(|c| c.actor != self.actor_str()) {
            let mut error = Error::new(
                Code::NotPermitted,
                "agent actors may only write with an active claim or, for notes and follow-ups, their waiting session",
            )
            .with_issue(issue_id);
            if self.session_claim(issue_id)?.is_some_and(|c| {
                c.actor == self.actor_str() && c.release_reason.as_deref() == Some("waiting")
            }) {
                error = error.with_hint(format!(
                    "waiting released your claim; ask a human to run `trk resume {issue_id} -m \"continuation instructions\"`; chat, restored access, and `trk send` do not restore ownership"
                ));
            }
            return Err(error);
        }
        Ok(())
    }

    /// Any write from the claimant extends its lease.
    pub fn touch_lease(&self, issue_id: &str) -> Result<()> {
        let until = fmt_ts(self.now + self.opts.agent_lease);
        self.conn.execute(
            "UPDATE claim SET lease_until = max(lease_until, ?3)
             WHERE issue_id = ?1 AND actor = ?2 AND released_at IS NULL AND lease_until IS NOT NULL",
            params![issue_id, self.actor_str(), until],
        )?;
        Ok(())
    }

    fn release(&self, claim: &Claim, reason: &str, extra: Value) -> Result<()> {
        self.conn.execute(
            "UPDATE claim SET released_at = ?2, release_reason = ?3 WHERE id = ?1",
            params![claim.id, self.ts(), reason],
        )?;
        let mut p = json!({"claim": claim.id, "claimant": claim.actor, "reason": reason});
        if let (Value::Object(p), Value::Object(e)) = (&mut p, extra) {
            p.extend(e);
        }
        self.event(Some(&claim.issue_id), "release", p)?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Create / edit
    // ------------------------------------------------------------------

    fn new_id(&self) -> Result<String> {
        let prefix = &self.opts.prefix;
        let mut rng = rand::rng();
        for len in 3..=8 {
            for _ in 0..8 {
                let suffix: String = (0..len)
                    .map(|_| ID_ALPHABET[rng.random_range(0..ID_ALPHABET.len())] as char)
                    .collect();
                let id = format!("{prefix}-{suffix}");
                if !self.issue_exists(&id)? {
                    return Ok(id);
                }
            }
        }
        Err(Error::new(Code::Internal, "could not allocate an issue id"))
    }

    fn issue_exists(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM issue WHERE id = ?1", [id], |_| Ok(true))
            .optional()?
            .unwrap_or(false))
    }

    pub fn create(&self, new: NewIssue) -> Result<String> {
        let title = new.title.trim().to_string();
        if title.is_empty() {
            return Err(Error::new(Code::InvalidField, "an issue needs a title")
                .with_hint("trk create -t TITLE [-d DESCRIPTION]"));
        }
        let priority = check_priority(new.priority.as_deref().unwrap_or(DEFAULT_PRIORITY))?;
        let issue_type = check_type(new.issue_type.as_deref().unwrap_or(DEFAULT_TYPE))?;
        let followup_of = new
            .followup_of
            .as_deref()
            .map(|p| self.resolve_id(p))
            .transpose()?;
        if self.actor().is_agent() {
            let ok = match &followup_of {
                Some(p) => self
                    .session_claim(p)?
                    .is_some_and(|c| c.actor == self.actor_str()),
                None => false,
            };
            if !ok {
                return Err(Error::new(
                    Code::NotPermitted,
                    "agent actors may create issues only as follow-ups of their active or waiting session",
                ));
            }
        }
        let blocked_by = new
            .blocked_by
            .iter()
            .map(|b| self.resolve_id(b))
            .collect::<Result<Vec<_>>>()?;
        let blocks = new
            .blocks
            .iter()
            .map(|b| self.resolve_id(b))
            .collect::<Result<Vec<_>>>()?;
        let id = match &new.id {
            Some(id) => {
                let ok = id
                    .strip_prefix(&format!("{}-", self.opts.prefix))
                    .is_some_and(|s| {
                        !s.is_empty()
                            && s.chars()
                                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                    });
                if !ok {
                    return Err(bad_args(format!(
                        "id '{id}' must be '{}-' followed by lowercase letters and digits",
                        self.opts.prefix
                    )));
                }
                if self.issue_exists(id)? {
                    return Err(bad_args(format!("issue '{id}' already exists")));
                }
                id.clone()
            }
            None => self.new_id()?,
        };
        self.conn.execute(
            "INSERT INTO issue (id, state, title, description, priority, type,
                                created_by, created_at, updated_at)
             VALUES (?1, 'open', ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                id,
                title,
                new.description,
                priority,
                issue_type,
                self.actor_str(),
                self.ts(),
            ],
        )?;
        self.event(
            Some(&id),
            "create",
            json!({
                "title": title,
                "description": new.description, "priority": priority, "type": issue_type,
                "followup_of": followup_of,
            }),
        )?;
        for b in &blocked_by {
            self.dep_insert(b, &id)?;
        }
        for b in &blocks {
            self.dep_insert(&id, b)?;
        }
        if let Some(p) = &followup_of {
            self.touch_lease(p)?;
        }
        Ok(id)
    }

    /// Change the authored fields. Returns `{field: {from, to}}`.
    pub fn edit(&self, id: &str, e: Edit) -> Result<Map<String, Value>> {
        let id = self.resolve_id(id)?;
        let mut issue = self.issue(&id)?;
        let claim = self.active_claim(&id)?;
        self.agent_gate(&id, &claim)?;
        let mut changes = Map::new();
        if let Some(t) = e.title {
            let t = t.trim().to_string();
            if t.is_empty() {
                return Err(Error::new(Code::InvalidField, "the title cannot be empty"));
            }
            if t != issue.title {
                changes.insert("title".into(), json!({"from": issue.title, "to": t}));
                issue.title = t;
            }
        }
        if let Some(d) = e.description
            && d != issue.description
        {
            changes.insert(
                "description".into(),
                json!({"from": issue.description, "to": d}),
            );
            issue.description = d;
        }
        if let Some(p) = e.priority {
            let p = check_priority(&p)?;
            if p != issue.priority {
                changes.insert("priority".into(), json!({"from": issue.priority, "to": p}));
                issue.priority = p;
            }
        }
        if let Some(t) = e.issue_type {
            let t = check_type(&t)?;
            if t != issue.issue_type {
                changes.insert("type".into(), json!({"from": issue.issue_type, "to": t}));
                issue.issue_type = t.to_owned();
            }
        }
        if changes.is_empty() {
            return Ok(changes);
        }
        self.save_issue(&issue)?;
        self.event(Some(&id), "set", json!({"changes": changes}))?;
        self.touch_lease(&id)?;
        Ok(changes)
    }

    // ------------------------------------------------------------------
    // Claims
    // ------------------------------------------------------------------

    /// Open blockers of an issue.
    pub fn blocked_by(&self, id: &str) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT d.src FROM dep d JOIN issue b ON b.id = d.src
             WHERE d.dst = ?1 AND b.state <> 'closed' ORDER BY d.src",
        )?;
        let v = st
            .query_map([id], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    pub fn claim(&self, id: &str, opts: ClaimOpts) -> Result<Claim> {
        let id = self.resolve_id(id)?;
        let mut issue = self.issue(&id)?;
        let me = self.actor_str();
        if let Some(obo) = &opts.on_behalf_of {
            self.ensure_actor(&me, Some(obo))?;
        }
        let existing = self.active_claim(&id)?;
        let lease = opts.lease.or(if self.actor().is_agent() {
            Some(self.opts.agent_lease)
        } else {
            None
        });
        let lease_until = lease.map(|d| fmt_ts(self.now + d));
        let mut stolen_from = None;
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
            self.release(c, "stolen", json!({"from": "claimed", "to": "claimed"}))?;
            stolen_from = Some(c.actor.clone());
        } else {
            if issue.state != State::Open {
                let hint = match issue.state {
                    State::Waiting | State::Closed => format!("trk reopen {id}"),
                    _ => String::new(),
                };
                let mut e = self.invalid_state(&issue, "only open issues can be claimed");
                if !hint.is_empty() {
                    e = e.with_hint(hint);
                }
                return Err(e);
            }
            let blockers = self.blocked_by(&id)?;
            if !blockers.is_empty() {
                return Err(Error::new(
                    Code::Blocked,
                    format!("{id} is blocked by {}", blockers.join(", ")),
                )
                .with_issue(&id)
                .with_detail("blocked_by", json!(blockers))
                .with_hint("close its blockers first, or remove the edge with `trk dep rm`"));
            }
            issue.state = State::Claimed;
        }
        issue.wake_count = 0;
        self.save_issue(&issue)?;
        self.conn.execute(
            "INSERT INTO claim (issue_id, actor, claimed_at, lease_until, worktree, host, agent, terminal)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, me, self.ts(), lease_until, opts.worktree, opts.host, opts.agent, opts.terminal],
        )?;
        let claim_id = self.conn.last_insert_rowid();
        let mut p = json!({
            "claim": claim_id, "lease_until": lease_until, "worktree": opts.worktree,
            "host": opts.host, "agent": opts.agent, "terminal": opts.terminal,
        });
        if let Some(s) = stolen_from {
            p["stolen_from"] = json!(s);
        }
        self.event(Some(&id), "claim", p)?;
        self.active_claim(&id)?
            .ok_or_else(|| Error::new(Code::Internal, "claim vanished"))
    }

    /// Bind the launching claimant's session after the terminal address is known.
    pub fn bind_terminal(&self, id: &str, terminal: &str) -> Result<()> {
        let id = self.resolve_id(id)?;
        let issue = self.issue(&id)?;
        let claim = self
            .active_claim(&id)?
            .ok_or_else(|| self.invalid_state(&issue, "it is not claimed"))?;
        if claim.actor != self.actor_str() {
            return Err(Error::new(
                Code::NotClaimant,
                format!("{id} is claimed by {}", claim.actor),
            )
            .with_issue(&id));
        }
        self.conn.execute(
            "UPDATE claim SET terminal = ?2 WHERE id = ?1",
            params![claim.id, terminal],
        )?;
        self.event(
            Some(&id),
            "set",
            json!({"claim": claim.id, "changes": {
                "claim.terminal": {"from": claim.terminal, "to": terminal}
            }}),
        )?;
        Ok(())
    }

    fn to_open(&self, issue: &mut Issue, claim: &Claim, extra: Value) -> Result<()> {
        issue.state = State::Open;
        self.save_issue(issue)?;
        let mut p = json!({"from": "claimed", "to": "open"});
        if let (Value::Object(p), Value::Object(e)) = (&mut p, extra) {
            p.extend(e);
        }
        self.release(claim, "unclaimed", p)
    }

    /// Release the active claim and return the issue to open.
    pub fn unclaim(&self, id: &str, force: bool, note: Option<&str>) -> Result<()> {
        let id = self.resolve_id(id)?;
        let mut issue = self.issue(&id)?;
        let claim = self.active_claim(&id)?;
        let Some(c) = claim.clone() else {
            return Err(self.invalid_state(&issue, "it is not claimed"));
        };
        self.agent_gate(&id, &claim)?;
        let forced = self.claimant_check(&issue, &claim, force, "unclaim")?;
        // The note goes first: it is written while the claim still exists.
        if let Some(n) = note {
            self.note(&id, n, None, false)?;
        }
        self.to_open(&mut issue, &c, json!({"forced": forced}))
    }

    /// Release a claim on behalf of `trk reap`.
    pub fn reap(&self, id: &str, why: &str) -> Result<()> {
        let claim = self
            .active_claim(id)?
            .ok_or_else(|| not_found("active claim on", id))?;
        let mut issue = self.issue(id)?;
        self.note(id, why, None, false)?;
        self.to_open(&mut issue, &claim, json!({"reaped": true}))
    }

    pub fn renew(&self, id: &str, lease: Option<jiff::SignedDuration>) -> Result<Claim> {
        let id = self.resolve_id(id)?;
        let issue = self.issue(&id)?;
        let c = self
            .active_claim(&id)?
            .ok_or_else(|| self.invalid_state(&issue, "it is not claimed"))?;
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
    // Close / wait / reopen
    // ------------------------------------------------------------------

    /// Close from open, claimed or waiting.
    pub fn close(&self, id: &str, reason: &str, note: Option<&str>, force: bool) -> Result<()> {
        let id = self.resolve_id(id)?;
        let mut issue = self.issue(&id)?;
        let claim = self.active_claim(&id)?;
        self.agent_gate(&id, &claim)?;
        if issue.is_closed() {
            return Err(self.invalid_state(&issue, "it is already closed"));
        }
        if !CLOSE_REASONS.contains(&reason) {
            return Err(Error::new(
                Code::InvalidField,
                format!("invalid close reason '{reason}'"),
            )
            .with_issue(&id)
            .with_detail("allowed", json!(CLOSE_REASONS)));
        }
        let forced = self.claimant_check(&issue, &claim, force, "close")?;
        if let Some(n) = note.filter(|n| !n.trim().is_empty()) {
            self.note(&id, n, None, false)?;
        }
        let from = issue.state;
        issue.state = State::Closed;
        issue.close_reason = Some(reason.to_string());
        issue.closed_at = Some(self.ts());
        Self::clear_waiting(&mut issue);
        self.save_issue(&issue)?;
        self.event(
            Some(&id),
            "close",
            json!({"from": from, "reason": reason, "forced": forced}),
        )?;
        if let Some(c) = claim {
            self.release(&c, "closed", json!({}))?;
        }
        Ok(())
    }

    /// End a session without closing: the issue waits for `check` to pass,
    /// or for a human when there is no check. On an issue that is already
    /// waiting this replaces the check, interval and reason.
    pub fn wait(&self, id: &str, opts: WaitOpts) -> Result<()> {
        let id = self.resolve_id(id)?;
        let mut issue = self.issue(&id)?;
        let claim = self.active_claim(&id)?;
        self.agent_gate(&id, &claim)?;
        let check = opts
            .check
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty());
        let reason = opts
            .reason
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty());
        if check.is_none() && reason.is_none() {
            return Err(Error::new(
                Code::InvalidField,
                "waiting needs a check command, a reason, or both",
            )
            .with_issue(&id)
            .with_hint(format!(
                "trk wait {id} --check 'CMD' [--every 5m] [--reason TEXT]"
            )));
        }
        if let Some(d) = opts.interval
            && d.as_secs() <= 0
        {
            return Err(Error::new(
                Code::InvalidField,
                "the check interval must be positive",
            ));
        }
        let interval = match (&check, opts.interval) {
            (Some(_), Some(d)) => Some(d.as_secs()),
            (Some(_), None) => Some(self.opts.check_interval.as_secs()),
            (None, _) => None,
        };
        let from = issue.state;
        let forced = match from {
            State::Claimed => self.claimant_check(&issue, &claim, opts.force, "set waiting")?,
            State::Waiting => false,
            _ => {
                return Err(self
                    .invalid_state(&issue, "only a claimed issue can start waiting")
                    .with_hint(format!("trk claim {id}")));
            }
        };
        if from == State::Claimed {
            issue.wait_cwd = claim.as_ref().and_then(|c| c.worktree.clone());
        }
        issue.state = State::Waiting;
        issue.waiting_reason = reason;
        issue.waiting_check = check;
        issue.check_interval = interval;
        // The first check is due at once: a merge should not wait an interval.
        issue.next_check_at = issue.waiting_check.as_ref().map(|_| self.ts());
        issue.last_check_at = None;
        issue.last_check_exit = None;
        self.save_issue(&issue)?;
        self.event(
            Some(&id),
            "wait",
            json!({
                "from": from, "reason": issue.waiting_reason, "check": issue.waiting_check,
                "interval": interval, "cwd": issue.wait_cwd, "forced": forced,
            }),
        )?;
        if let Some(c) = claim {
            self.release(&c, "waiting", json!({}))?;
        }
        Ok(())
    }

    /// Return a waiting or closed issue to open.
    pub fn reopen(&self, id: &str, note: Option<&str>) -> Result<()> {
        let id = self.resolve_id(id)?;
        let mut issue = self.issue(&id)?;
        self.agent_gate(&id, &None)?;
        if !matches!(issue.state, State::Waiting | State::Closed) {
            return Err(self.invalid_state(&issue, "only waiting or closed issues can be reopened"));
        }
        if let Some(n) = note.filter(|n| !n.trim().is_empty()) {
            self.note(&id, n, None, false)?;
        }
        let from = issue.state;
        issue.state = State::Open;
        issue.close_reason = None;
        issue.closed_at = None;
        Self::clear_waiting(&mut issue);
        self.save_issue(&issue)?;
        self.event(Some(&id), "reopen", json!({"from": from}))?;
        Ok(())
    }

    /// Human authorization to continue the original waiting session. The caller
    /// must hold the check queue lock and establish liveness before calling.
    pub fn resume(
        &self,
        id: &str,
        previous_claim: i64,
        revision: i64,
        message: &str,
    ) -> Result<Claim> {
        self.resume_human_gate(id)?;
        if message.trim().is_empty() {
            return Err(bad_args("resume needs a nonempty continuation message"));
        }
        let mut issue = self.issue(id)?;
        if issue.state != State::Waiting
            || self.active_claim(id)?.is_some()
            || self.issue_revision(id)? != revision
        {
            return Err(self.invalid_state(&issue, "waiting session changed; resume refused"));
        }
        let previous = self
            .session_claim(id)?
            .filter(|c| {
                c.id == previous_claim
                    && c.release_reason.as_deref() == Some("waiting")
                    && c.released_at.is_some()
                    && c.terminal.as_ref().is_some_and(|t| !t.trim().is_empty())
                    && ActorId::parse(&c.actor).is_ok_and(|a| a.is_agent())
            })
            .ok_or_else(|| {
                self.invalid_state(&issue, "original waiting session changed or is absent")
            })?;
        self.note(
            id,
            &format!(
                "Human authorized resuming the original waiting session:\n\n{}",
                message.trim()
            ),
            None,
            false,
        )?;
        issue.state = State::Claimed;
        Self::clear_waiting(&mut issue);
        self.save_issue(&issue)?;
        self.restore_waiting_claim(id, &previous, false)?;
        self.active_claim(id)?
            .ok_or_else(|| Error::new(Code::Internal, "restored claim vanished"))
    }

    fn resume_human_gate(&self, id: &str) -> Result<()> {
        if self.actor().is_agent() {
            return Err(Error::new(Code::NotPermitted, "only a human may authorize trk resume")
                .with_issue(id)
                .with_hint("ask the human to run `trk resume <ID> -m \"continuation instructions\"`; chat or restored access alone does not restore a claim"));
        }
        Ok(())
    }

    /// Record delivery even if the session has already ended its new cycle.
    pub fn resume_delivered(&self, id: &str, claim: i64) -> Result<()> {
        self.resume_human_gate(id)?;
        self.event(
            Some(id),
            "note",
            json!({"body": format!("Human-authorized resume delivered to the original session (claim {claim})."),
                   "resume": {"result": "delivered", "claim": claim}}),
        )?;
        Ok(())
    }

    /// Compensate only the exact claim restored by this authorization.
    pub fn resume_failed(&self, id: &str, claim: i64, error: &str) -> Result<bool> {
        self.resume_human_gate(id)?;
        let reopened =
            self.restoration_failed(id, claim, None, json!({"resume_delivery_failed": true}))?;
        self.event(
            Some(id),
            "note",
            json!({
                "body": format!("Human-authorized resume delivery failed for claim {claim}: {error}. {}",
                    if reopened { "The restored claim was released; the issue is open again." }
                    else { "The restored claim already changed; no replacement state was modified." }),
                "resume": {"result": "delivery_failed", "claim": claim, "error": error, "reopened": reopened}
            }),
        )?;
        Ok(reopened)
    }

    /// A lifecycle revision avoids ABA races, including a new wait on the same
    /// command within one clock tick. Notes do not invalidate a running check.
    pub fn issue_revision(&self, id: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT coalesce(max(seq), 0) FROM event WHERE issue_id = ?1
             AND op IN ('wait', 'claim', 'release', 'close', 'reopen', 'set')",
            [id],
            |r| r.get(0),
        )?)
    }

    /// Shared restoration for automatic check wakes and human continuation.
    fn restore_waiting_claim(&self, id: &str, previous: &Claim, by_check: bool) -> Result<i64> {
        let lease_until = fmt_ts(self.now + self.opts.agent_lease);
        self.conn.execute(
            "INSERT INTO claim
             (issue_id, actor, claimed_at, lease_until, worktree, host, agent, terminal)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                previous.actor,
                self.ts(),
                lease_until,
                previous.worktree,
                previous.host,
                previous.agent,
                previous.terminal
            ],
        )?;
        let claim = self.conn.last_insert_rowid();
        self.event(
            Some(id),
            "claim",
            json!({
                "claim": claim, "claimant": previous.actor, "lease_until": lease_until,
                "worktree": previous.worktree, "host": previous.host, "agent": previous.agent,
                "terminal": previous.terminal, "from": "waiting", "by_check": by_check,
                "resumed_from": previous.id,
            }),
        )?;
        Ok(claim)
    }

    // ------------------------------------------------------------------
    // The waiting queue
    // ------------------------------------------------------------------

    /// Waiting issues with a check, in the order `trk check` runs them:
    /// priority, then due time. `all` ignores the due time.
    pub fn check_queue(&self, all: bool) -> Result<Vec<Issue>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {ISSUE_COLS} FROM issue i
             WHERE i.state = 'waiting' AND i.waiting_check IS NOT NULL
               AND (?1 OR i.next_check_at IS NULL OR i.next_check_at <= ?2)
             ORDER BY i.priority, i.next_check_at, i.id"
        ))?;
        let v = st
            .query_map(params![all, self.ts()], row_to_issue)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    /// The runner pid, host, start time and active subprocess group, if any.
    /// Ownership must remain until both the runner and its subprocess group are gone.
    pub fn check_lock(&self) -> Result<Option<(i64, Option<String>, String, Option<i64>)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT pid, host, started_at, pgid FROM check_lock WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?)
    }

    /// Take the check lock, replacing its holder and clearing its subprocess group.
    /// The caller must first establish that the old holder and group are gone.
    pub fn check_lock_take(&self, pid: i64, host: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO check_lock (id, pid, host, started_at) VALUES (1, ?1, ?2, ?3)",
            params![pid, host, self.ts()],
        )?;
        Ok(())
    }

    /// Set or clear the subprocess group only if `pid` still owns the lock.
    pub fn check_lock_group(&self, pid: i64, pgid: Option<i64>) -> Result<()> {
        self.conn.execute(
            "UPDATE check_lock SET pgid = ?1 WHERE id = 1 AND pid = ?2",
            params![pgid, pid],
        )?;
        Ok(())
    }

    /// Release only the matching holder, after its subprocess group is gone.
    pub fn check_lock_release(&self, pid: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM check_lock WHERE id = 1 AND pid = ?1", [pid])?;
        Ok(())
    }

    /// Record one run of an issue's check. The command and lifecycle revision
    /// must still match the snapshot taken before execution, even if a new
    /// waiting cycle reused the same command. A failed check may restore only
    /// the latest waiting claim after the caller establishes session liveness.
    pub fn check_record(
        &self,
        id: &str,
        check: &str,
        revision: i64,
        run: &CheckRun,
        resume_claim: Option<i64>,
    ) -> Result<CheckResult> {
        let mut issue = self.issue(id)?;
        if issue.state != State::Waiting
            || issue.waiting_check.as_deref() != Some(check)
            || self.issue_revision(id)? != revision
            || self.active_claim(id)?.is_some()
        {
            return Ok(CheckResult::Skipped);
        }
        if run.exit == Some(CHECK_RETRY_EXIT) && !run.timed_out {
            let every = issue
                .check_interval
                .unwrap_or_else(|| self.opts.check_interval.as_secs());
            issue.last_check_at = Some(self.ts());
            issue.last_check_exit = Some(i64::from(CHECK_RETRY_EXIT));
            issue.next_check_at = Some(fmt_ts(self.now + jiff::SignedDuration::from_secs(every)));
            // Not an event: a long wait would otherwise flood the history.
            // `updated_at` is left alone for the same reason.
            self.conn.execute(
                "UPDATE issue SET last_check_at = ?2, last_check_exit = ?3, next_check_at = ?4
                 WHERE id = ?1",
                params![
                    id,
                    issue.last_check_at,
                    issue.last_check_exit,
                    issue.next_check_at
                ],
            )?;
            return Ok(CheckResult::Retry);
        }
        let reason = issue.waiting_reason.clone();
        if run.exit == Some(0) && !run.timed_out {
            issue.state = State::Closed;
            issue.close_reason = Some("done".into());
            issue.closed_at = Some(self.ts());
            Self::clear_waiting(&mut issue);
            self.save_issue(&issue)?;
            self.event(
                Some(id),
                "check",
                json!({"result": "passed", "exit": 0, "check": check, "reason": reason}),
            )?;
            self.event(
                Some(id),
                "close",
                json!({"from": "waiting", "reason": "done", "by_check": true}),
            )?;
            return Ok(CheckResult::Passed);
        }
        let capped = issue.wake_count >= self.opts.max_wakes;
        let resume = if let Some(claim) = resume_claim.filter(|_| !capped) {
            self.session_claim(id)?.filter(|c| {
                c.id == claim
                    && c.released_at.is_some()
                    && c.release_reason.as_deref() == Some("waiting")
            })
        } else {
            None
        };
        let what = if run.timed_out {
            "timed out".to_string()
        } else {
            match run.exit {
                Some(c) => format!("exited with {c}"),
                None => "did not run to completion".to_string(),
            }
        };
        let out = tail(run.output.trim_end(), CHECK_OUTPUT_TAIL);
        let outcome = if resume.is_some() {
            "the waiting session has been resumed"
        } else {
            "the issue is open again"
        };
        let mut body = format!("The waiting check {what}; {outcome}.\n");
        if capped {
            body.push_str(&format!(
                "\nSession wake cap of {} reached; no further wake was sent.\n",
                self.opts.max_wakes,
            ));
        }
        if let Some(r) = &reason {
            body.push_str(&format!("\nIt was waiting for: {r}\n"));
        }
        if let Some(cwd) = &issue.wait_cwd {
            body.push_str(&format!("\nDirectory: `{cwd}`\n"));
        }
        body.push_str(&format!("\nCheck:\n\n```sh\n{check}\n```\n"));
        if out.is_empty() {
            body.push_str("\nIt printed nothing.\n");
        } else {
            body.push_str(&format!("\nOutput:\n\n```\n{out}\n```\n"));
        }
        self.note(id, &body, None, false)?;
        issue.state = if resume.is_some() {
            State::Claimed
        } else {
            State::Open
        };
        if resume.is_some() {
            issue.wake_count += 1;
        }
        Self::clear_waiting(&mut issue);
        self.save_issue(&issue)?;
        self.event(
            Some(id),
            "check",
            json!({
                "result": "failed", "exit": run.exit, "timed_out": run.timed_out,
                "check": check, "reason": reason,
            }),
        )?;
        if let Some(previous) = resume {
            let claim = self.restore_waiting_claim(id, &previous, true)?;
            return Ok(CheckResult::Woken { claim });
        }
        self.event(
            Some(id),
            "reopen",
            json!({"from": "waiting", "by_check": true}),
        )?;
        Ok(CheckResult::Failed)
    }

    /// Reopen after delivery to a restored session failed, but only while that
    /// exact claim is still active. A session that moved on must not be changed.
    pub fn check_wake_failed(&self, id: &str, claim_id: i64) -> Result<bool> {
        self.restoration_failed(
            id,
            claim_id,
            Some("The failed waiting check could not be delivered to the resumed session; the issue is open again."),
            json!({"check_wake_failed": true}),
        )
    }

    fn restoration_failed(
        &self,
        id: &str,
        claim_id: i64,
        note: Option<&str>,
        extra: Value,
    ) -> Result<bool> {
        let mut issue = self.issue(id)?;
        if issue.state != State::Claimed {
            return Ok(false);
        }
        let Some(claim) = self.active_claim(id)?.filter(|c| c.id == claim_id) else {
            return Ok(false);
        };
        if let Some(note) = note {
            self.note(id, note, None, false)?;
        }
        self.to_open(&mut issue, &claim, extra)?;
        Ok(true)
    }

    // ------------------------------------------------------------------
    // Notes
    // ------------------------------------------------------------------

    pub fn note(&self, id: &str, body: &str, amends: Option<i64>, to_agent: bool) -> Result<i64> {
        let id = self.resolve_id(id)?;
        let claim = self.session_claim(&id)?;
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
        self.touch_lease(&id)?;
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
    // Dependencies: `blocker` blocks `blocked`
    // ------------------------------------------------------------------

    fn dep_insert(&self, blocker: &str, blocked: &str) -> Result<bool> {
        if blocker == blocked {
            return Err(Error::new(Code::Cycle, "an issue cannot block itself"));
        }
        self.issue(blocker)?;
        self.issue(blocked)?;
        let exists: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM dep WHERE src = ?1 AND dst = ?2",
                params![blocker, blocked],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if exists {
            return Ok(false);
        }
        // Refused if the blocker is already reachable from the blocked issue.
        let cyc: bool = self
            .conn
            .query_row(
                "WITH RECURSIVE reach(id) AS (
                   SELECT ?2
                   UNION
                   SELECT d.dst FROM dep d JOIN reach r ON d.src = r.id
                 )
                 SELECT 1 FROM reach WHERE id = ?1 LIMIT 1",
                params![blocker, blocked],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if cyc {
            return Err(Error::new(
                Code::Cycle,
                format!("{blocker} blocks {blocked} would create a cycle"),
            ));
        }
        self.conn.execute(
            "INSERT INTO dep (src, dst, created_by, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![blocker, blocked, self.actor_str(), self.ts()],
        )?;
        let p = json!({"blocker": blocker, "blocked": blocked});
        self.event(Some(blocker), "dep_add", p.clone())?;
        self.event(Some(blocked), "dep_add", p)?;
        self.conn.execute(
            "UPDATE issue SET updated_at = ?3 WHERE id IN (?1, ?2)",
            params![blocker, blocked, self.ts()],
        )?;
        Ok(true)
    }

    fn dep_authorize(&self, a: &str, b: &str) -> Result<()> {
        if self.actor().is_agent() && !self.is_claimed_by_me(a)? && !self.is_claimed_by_me(b)? {
            return Err(Error::new(
                Code::NotPermitted,
                "agent actors may only add or remove edges touching an issue they claim",
            ));
        }
        Ok(())
    }

    /// `blocker` blocks `blocked`. Returns false when the edge already existed.
    pub fn dep_add(&self, blocker: &str, blocked: &str) -> Result<bool> {
        let blocker = self.resolve_id(blocker)?;
        let blocked = self.resolve_id(blocked)?;
        self.dep_authorize(&blocker, &blocked)?;
        let added = self.dep_insert(&blocker, &blocked)?;
        self.touch_lease(&blocker)?;
        self.touch_lease(&blocked)?;
        Ok(added)
    }

    pub fn dep_rm(&self, blocker: &str, blocked: &str) -> Result<()> {
        let blocker = self.resolve_id(blocker)?;
        let blocked = self.resolve_id(blocked)?;
        self.dep_authorize(&blocker, &blocked)?;
        let n = self.conn.execute(
            "DELETE FROM dep WHERE src = ?1 AND dst = ?2",
            params![blocker, blocked],
        )?;
        if n == 0 {
            return Err(Error::new(
                Code::NotFound,
                format!("{blocker} does not block {blocked}"),
            ));
        }
        let p = json!({"blocker": blocker, "blocked": blocked});
        self.event(Some(&blocker), "dep_rm", p.clone())?;
        self.event(Some(&blocked), "dep_rm", p)?;
        self.conn.execute(
            "UPDATE issue SET updated_at = ?3 WHERE id IN (?1, ?2)",
            params![blocker, blocked, self.ts()],
        )?;
        Ok(())
    }
}
