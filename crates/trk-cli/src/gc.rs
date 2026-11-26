//! Conservative cleanup of resources left by closed issues.

mod legacy_hc;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde::Serialize;
use serde_json::{Value, json};
use trk_core::config::parse_duration;
use trk_core::error::{Code, Error, Result};
use trk_core::model::{Claim, State, parse_ts};
use trk_core::store::Ctx;
use trk_core::system::hostname;

use crate::env::Env;
use crate::herdr;
use crate::render::Out;

#[derive(Serialize)]
pub(crate) struct Resource {
    status: &'static str,
    reason: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    items: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    space: Option<String>,
}

impl Resource {
    pub(crate) fn new(status: &'static str, reason: impl Into<String>) -> Self {
        Self {
            status,
            reason: reason.into(),
            items: Vec::new(),
            space: None,
        }
    }

    pub(crate) fn in_space(mut self, space: &str) -> Self {
        self.space = Some(space.to_owned());
        self
    }

    fn summary(items: Vec<Value>, empty: &str) -> Self {
        let status = if items.iter().any(|v| v["status"] == "removed") {
            "removed"
        } else if items.iter().any(|v| v["status"] == "skipped") {
            "skipped"
        } else {
            "absent"
        };
        let reason = if items.is_empty() {
            empty.to_owned()
        } else {
            items
                .iter()
                .map(|v| v["reason"].as_str().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("; ")
        };
        Self {
            status,
            reason,
            items,
            space: None,
        }
    }

    fn collected(&self) -> bool {
        self.status != "skipped" && self.items.iter().all(|item| item["status"] != "skipped")
    }
}

#[derive(Serialize)]
struct IssueResult {
    id: String,
    session: Resource,
    worktree: Resource,
    branch: Resource,
}

struct Plan {
    id: String,
    revision: i64,
    claims: Vec<(Claim, bool)>,
    gate: Option<&'static str>,
}

impl IssueResult {
    fn skipped(id: String, reason: &str) -> Self {
        Self {
            id,
            session: Resource::new("skipped", reason),
            worktree: Resource::new("skipped", reason),
            branch: Resource::new("skipped", reason),
        }
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<Output> {
    Ok(Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?)
}

fn diagnostic(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
    .trim()
    .to_owned()
}

fn checked_git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = git(repo, args)?;
    if !out.status.success() {
        return Err(Error::new(
            Code::InvalidState,
            format!("git {}: {}", args.join(" "), diagnostic(&out)),
        ));
    }
    Ok(out.stdout)
}

/// Canonicalize existing ancestors too: an absent child of a symlink must not
/// evade the configured-directory boundary. Unresolvable paths are never removed.
fn resolved(path: &Path) -> std::io::Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(p) => Ok(p),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .filter(|p| *p != path)
                .ok_or_else(|| std::io::Error::other("path has no resolvable parent"))?;
            let name = path
                .file_name()
                .ok_or_else(|| std::io::Error::other("path is not normalized"))?;
            Ok(resolved(parent)?.join(name))
        }
        Err(e) => Err(e),
    }
}

fn host_matches(claim: &Claim, host: Option<&str>) -> bool {
    host.is_some() && claim.host.as_deref() == host
}

#[expect(
    clippy::too_many_arguments,
    reason = "Keep host/protection gates and per-collection recovery state explicit at the cleanup boundary"
)]
fn session(
    env: &Env,
    claim: &Claim,
    recovered: bool,
    host: Option<&str>,
    protected: &[String],
    dry_run: bool,
    recovery: &mut legacy_hc::Recovery,
    closed_spaces: &mut BTreeSet<String>,
) -> Result<Resource> {
    if !host_matches(claim, host) {
        return Ok(Resource::new(
            "skipped",
            "terminal belongs to another or unknown host",
        ));
    }
    let Some(launcher) = crate::launcher::for_claim(env, claim.launcher.as_deref())? else {
        return legacy_hc::recover(env, recovered, claim, protected, dry_run, recovery);
    };
    let terminal = claim.terminal.as_deref().expect("filtered terminal");
    if launcher.is_herdr() {
        return herdr::close_session(terminal, protected, dry_run, closed_spaces);
    }
    let Some(close) = launcher.close.as_deref().filter(|close| !close.is_empty()) else {
        return Ok(Resource::new(
            "skipped",
            "launcher does not support closing terminals",
        ));
    };
    if launcher.exists(terminal) == Some(false) {
        return Ok(Resource::new("absent", "terminal is already gone"));
    }
    if !dry_run
        && let Some((false, output)) = launcher.operation(Some(close), terminal, None, false)?
    {
        return Err(Error::new(
            Code::InvalidState,
            format!("launcher close failed: {output}"),
        ));
    }
    Ok(Resource::new(
        "removed",
        format!(
            "{} terminal {terminal}",
            if dry_run { "would remove" } else { "removed" }
        ),
    ))
}

fn worktree(repo: &Path, path: &Path, root: &Path, cwd: &Path, dry_run: bool) -> Result<Resource> {
    // Absence is not a deletion, even outside today's configured root.
    if !path.try_exists()? {
        return Ok(Resource::new("absent", "worktree is already absent"));
    }
    if path == root || !path.starts_with(root) {
        return Ok(Resource::new(
            "skipped",
            "worktree is outside the configured worktrees directory",
        ));
    }
    if cwd.starts_with(path) {
        return Ok(Resource::new(
            "skipped",
            "worktree contains the current working directory",
        ));
    }
    let list = checked_git(repo, &["worktree", "list", "--porcelain", "-z"])?;
    let mut registered = false;
    let mut locked = false;
    let mut matching = false;
    for field in list.split(|b| *b == 0) {
        if let Some(p) = field.strip_prefix(b"worktree ") {
            matching = resolved(Path::new(
                std::str::from_utf8(p)
                    .map_err(|e| Error::new(Code::InvalidState, e.to_string()))?,
            ))
            .is_ok_and(|p| p == path);
            registered |= matching;
        } else if matching && (field == b"locked" || field.starts_with(b"locked ")) {
            locked = true;
        }
    }
    if !registered {
        return Ok(Resource::new(
            "skipped",
            "path is not a registered worktree of this repository",
        ));
    }
    if locked {
        return Ok(Resource::new("skipped", "git worktree is locked"));
    }
    let p = path
        .to_str()
        .ok_or_else(|| Error::new(Code::InvalidState, "non-UTF-8 worktree path"))?;
    let status = checked_git(
        path,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?;
    if !status.is_empty() {
        return Ok(Resource::new(
            "skipped",
            "worktree has modified or untracked files",
        ));
    }
    if !dry_run {
        let out = git(repo, &["worktree", "remove", "--", p])?;
        if !out.status.success() {
            let reason = diagnostic(&out);
            // Git also refuses clean worktrees containing submodules without force.
            if reason.contains("contains submodules")
                || reason.contains("modified or untracked")
                || reason.contains("locked")
            {
                return Ok(Resource::new("skipped", reason));
            }
            return Err(Error::new(
                Code::InvalidState,
                format!("git worktree remove: {reason}"),
            ));
        }
    } else if path.join(".gitmodules").exists() {
        return Ok(Resource::new(
            "skipped",
            "worktree contains submodules; git may require force",
        ));
    }
    Ok(Resource::new(
        "removed",
        format!(
            "{} worktree {p}",
            if dry_run { "would remove" } else { "removed" }
        ),
    ))
}

fn branch(
    repo: &Path,
    id: &str,
    worktrees_clear: bool,
    planned_paths: &BTreeSet<PathBuf>,
    dry_run: bool,
) -> Result<Resource> {
    let reference = format!("refs/heads/{id}");
    let exists = git(repo, &["show-ref", "--verify", "--quiet", &reference])?;
    if exists.status.code() == Some(1) {
        return Ok(Resource::new("absent", "branch is already absent"));
    }
    if !exists.status.success() {
        return Err(Error::new(
            Code::InvalidState,
            format!("cannot inspect branch: {}", diagnostic(&exists)),
        ));
    }
    if !worktrees_clear {
        return Ok(Resource::new(
            "skipped",
            "worktree was not removed or already absent",
        ));
    }
    if dry_run {
        // branch -d checks the upstream when configured, otherwise HEAD.
        let upstream = git(
            repo,
            &["rev-parse", "--verify", &format!("{id}@{{upstream}}")],
        )?;
        let upstream = String::from_utf8_lossy(&upstream.stdout);
        let base = if upstream.trim().is_empty() {
            "HEAD"
        } else {
            upstream.trim()
        };
        let merged = git(repo, &["merge-base", "--is-ancestor", &reference, base])?;
        if merged.status.code() == Some(1) {
            return Ok(Resource::new("skipped", "branch is not fully merged"));
        }
        if !merged.status.success() {
            return Err(Error::new(Code::InvalidState, diagnostic(&merged)));
        }
        let list = checked_git(repo, &["worktree", "list", "--porcelain", "-z"])?;
        let branch_field = format!("branch {reference}");
        let mut planned = false;
        for field in list.split(|b| *b == 0) {
            if let Some(path) = field.strip_prefix(b"worktree ") {
                planned = std::str::from_utf8(path)
                    .ok()
                    .and_then(|path| resolved(Path::new(path)).ok())
                    .is_some_and(|path| planned_paths.contains(&path));
            } else if field == branch_field.as_bytes() && !planned {
                return Ok(Resource::new(
                    "skipped",
                    "branch is still checked out in a retained worktree",
                ));
            }
        }
    } else {
        let out = git(repo, &["branch", "-d", "--", id])?;
        if !out.status.success() {
            let reason = diagnostic(&out);
            if reason.contains("not fully merged")
                || reason.contains("used by worktree")
                || reason.contains("checked out")
            {
                return Ok(Resource::new("skipped", reason));
            }
            return Err(Error::new(
                Code::InvalidState,
                format!("git branch -d: {reason}"),
            ));
        }
    }
    Ok(Resource::new(
        "removed",
        format!(
            "{} branch {id}",
            if dry_run { "would remove" } else { "removed" }
        ),
    ))
}

/// Collect under the caller's queue lock. Database planning, per-issue
/// reservation and result recording are short transactions; effects are not.
pub(crate) fn collect(
    env: &Env,
    ids: &[String],
    dry_run: bool,
    abandoned: bool,
    older_than: Option<&str>,
    sweep: bool,
) -> Result<Value> {
    collect_with_recovery(env, ids, dry_run, abandoned, older_than, sweep, false)
}

fn collect_with_recovery(
    env: &Env,
    ids: &[String],
    dry_run: bool,
    abandoned: bool,
    older_than: Option<&str>,
    sweep: bool,
    legacy_hc: bool,
) -> Result<Value> {
    let age = older_than.map(parse_duration).transpose()?;
    let actor = env.actor()?;
    let mut store = env.store()?;
    let plans = store.write(actor, dry_run, |c| {
        let ids = if ids.is_empty() {
            let query = if sweep {
                "SELECT id FROM issue WHERE state = 'closed'
                 AND gc_collected_at IS NULL ORDER BY id"
            } else {
                "SELECT id FROM issue WHERE state = 'closed' ORDER BY id"
            };
            let mut stmt = c.conn.prepare(query)?;
            stmt.query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            ids.iter()
                .map(|id| c.resolve_id(id))
                .collect::<Result<BTreeSet<_>>>()?
                .into_iter()
                .collect()
        };
        ids.into_iter()
            .map(|id| {
                let gate = eligibility(c, &id, abandoned, age)?;
                let claims = if gate.is_none() {
                    c.claims(&id)?
                        .into_iter()
                        .map(|claim| {
                            let recovered = claim.launcher.as_deref().is_none_or(str::is_empty)
                                && legacy_hc::recorded(c, &claim)?;
                            Ok((claim, recovered))
                        })
                        .collect::<Result<Vec<_>>>()?
                } else {
                    Vec::new()
                };
                Ok(Plan {
                    revision: c.issue_revision(&id)?,
                    id,
                    claims,
                    gate,
                })
            })
            .collect::<Result<Vec<_>>>()
    })?;
    let mut results = Vec::new();
    let mut errors = Vec::new();
    // Avoid resource probes altogether when the sweep has no pending issues.
    if plans.iter().any(|plan| plan.gate.is_none()) {
        let host = hostname();
        let cwd = std::fs::canonicalize(std::env::current_dir()?)?;
        let root = resolved(&env.worktrees_dir())?;
        let repo = env.checkout();
        let pid = i64::from(std::process::id());
        let mut recovery = legacy_hc::Recovery::new(legacy_hc);
        let mut closed_spaces = BTreeSet::new();
        for plan in plans {
            let Plan {
                id,
                revision,
                claims,
                gate,
            } = plan;
            if let Some(reason) = gate {
                if !sweep {
                    results.push(IssueResult::skipped(id, reason));
                }
                continue;
            }
            // Reopening/reclaiming after the snapshot invalidates the plan,
            // even if the issue was closed again within the same clock tick.
            // The reservation fences this issue, not unrelated writers, until
            // its external effects and result recording have finished.
            let protected = store.write(actor, dry_run, |c| {
                if c.issue_revision(&id)? != revision
                    || eligibility(c, &id, abandoned, age)?.is_some()
                {
                    return Ok(None);
                }
                if !dry_run && !c.gc_reserve(&id, revision, pid)? {
                    return Ok(None);
                }
                Ok(Some(protected_terminals(c, &id, host)?))
            })?;
            let Some(protected) = protected else {
                if !sweep {
                    results.push(IssueResult::skipped(id, "issue changed since gc planning"));
                }
                continue;
            };
            let mut sessions = Vec::new();
            let mut paths = Vec::new();
            let mut seen_terminals = BTreeSet::new();
            let mut seen_paths = BTreeSet::new();
            let mut removed = Vec::new();
            let mut clear = true;
            let mut planned_paths = BTreeSet::new();
            for (claim, recovered) in &claims {
                if let Some(terminal) = &claim.terminal
                    && seen_terminals.insert((claim.host.as_deref(), terminal))
                {
                    let resource = capture(
                        session(
                            env,
                            claim,
                            *recovered,
                            host,
                            &protected,
                            dry_run,
                            &mut recovery,
                            &mut closed_spaces,
                        ),
                        &mut errors,
                    );
                    if resource.status == "removed" {
                        removed.push(resource.reason.clone());
                    }
                    let mut item = serde_json::to_value(resource)?;
                    item["terminal"] = json!(terminal);
                    sessions.push(item);
                }
                if let Some(path) = &claim.worktree
                    && seen_paths.insert((claim.host.as_deref(), path))
                {
                    let resource = if !host_matches(claim, host) {
                        Resource::new("skipped", "worktree belongs to another or unknown host")
                    } else if let Some(repo) = &repo {
                        match resolved(Path::new(path)) {
                            // Historical worktreeless claims recorded their cwd.
                            // The primary checkout is not an issue-owned resource.
                            Ok(path) if path == *repo => continue,
                            Ok(path) => {
                                capture(worktree(repo, &path, &root, &cwd, dry_run), &mut errors)
                            }
                            Err(e) => Resource::new(
                                "skipped",
                                format!("cannot resolve worktree path: {e}"),
                            ),
                        }
                    } else {
                        Resource::new("skipped", "project has no primary git checkout")
                    };
                    clear &= resource.status != "skipped";
                    if resource.status == "removed" {
                        removed.push(resource.reason.clone());
                        if let Ok(path) = resolved(Path::new(path)) {
                            planned_paths.insert(path);
                        }
                    }
                    let mut item = serde_json::to_value(resource)?;
                    item["path"] = json!(path);
                    paths.push(item);
                }
            }
            // Only recorded issue worktrees establish branch ownership, not a
            // primary-checkout cwd, today's type configuration or a matching name.
            let branch = if paths.is_empty() {
                Resource::new("absent", "no recorded worktrees or associated branch")
            } else if let Some(repo) = &repo {
                capture(
                    branch(repo, &id, clear, &planned_paths, dry_run),
                    &mut errors,
                )
            } else {
                Resource::new("skipped", "project has no primary git checkout")
            };
            if branch.status == "removed" {
                removed.push(branch.reason.clone());
            }
            let result = IssueResult {
                id,
                session: Resource::summary(sessions, "no recorded terminals"),
                worktree: Resource::summary(paths, "no recorded worktrees"),
                branch,
            };
            if !dry_run {
                store.write(actor, false, |c| {
                    let current = c.issue_revision(&result.id)? == revision
                        && eligibility(c, &result.id, abandoned, age)?.is_none();
                    for payload in recovery.receipts.drain(..) {
                        c.event(Some(&result.id), "note", payload)?;
                    }
                    if !removed.is_empty() {
                        c.event(
                            Some(&result.id),
                            "note",
                            json!({"body": format!("trk gc: {}.", removed.join("; "))}),
                        )?;
                    }
                    c.gc_finish(
                        &result.id,
                        pid,
                        current
                            && result.session.collected()
                            && result.worktree.collected()
                            && result.branch.collected(),
                    )?;
                    Ok(())
                })?;
                recovery.receipts.clear();
            }
            results.push(result);
        }
    } else if !sweep {
        results.extend(
            plans
                .into_iter()
                .map(|plan| IssueResult::skipped(plan.id, plan.gate.expect("ineligible plan"))),
        );
    }
    let mut data = json!({"dry_run": dry_run, "issues": results});
    if !errors.is_empty() {
        data["errors"] = json!(errors);
    }
    Ok(data)
}

fn eligibility(
    c: &Ctx<'_>,
    id: &str,
    abandoned: bool,
    age: Option<jiff::SignedDuration>,
) -> Result<Option<&'static str>> {
    let issue = c.issue(id)?;
    Ok(if issue.state != State::Closed {
        Some("issue is not closed")
    } else if c.active_claim(id)?.is_some() {
        Some("issue has an active claim")
    } else if issue.waiting_reason.is_some() || issue.waiting_check.is_some() {
        Some("issue has waiting state")
    } else if issue.close_reason.as_deref() != Some("done")
        && !(abandoned && issue.close_reason.as_deref() == Some("abandoned"))
    {
        Some("issue was abandoned; use --abandoned")
    } else if age.is_some_and(|age| {
        issue
            .closed_at
            .as_deref()
            .and_then(parse_ts)
            .is_none_or(|at| c.now - at < age)
    }) {
        Some("issue closed more recently than --older-than (or closure time is unknown)")
    } else {
        None
    })
}

pub fn run(
    env: &Env,
    ids: Vec<String>,
    dry_run: bool,
    abandoned: bool,
    older_than: Option<&str>,
    legacy_hc: bool,
) -> Result<Out> {
    if legacy_hc && ids.is_empty() {
        return Err(Error::new(
            Code::InvalidState,
            "--legacy-hc requires explicit issue IDs and independently verified hc provenance",
        ));
    }
    let data = crate::check::with_gc_lock(env, dry_run, || {
        collect_with_recovery(env, &ids, dry_run, abandoned, older_than, false, legacy_hc)
    })?;
    if let Some(errors) = data["errors"].as_array() {
        let message = errors
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("; ");
        return Err(Error::new(Code::InvalidState, message).with_detail("gc", data));
    }
    let issues = data["issues"].as_array().unwrap();
    let mut text = String::new();
    for issue in issues {
        text.push_str(&format!(
            "{}{}\n",
            issue["id"].as_str().unwrap_or_default(),
            if dry_run { " (dry-run)" } else { "" }
        ));
        for (name, resource) in [
            ("session", &issue["session"]),
            ("worktree", &issue["worktree"]),
            ("branch", &issue["branch"]),
        ] {
            text.push_str(&format!(
                "  {name}: {} — {}\n",
                resource["status"].as_str().unwrap_or_default(),
                resource["reason"].as_str().unwrap_or_default()
            ));
        }
    }
    if issues.is_empty() {
        text.push_str("No closed issues to clean up.\n");
    }
    Ok(Out::Text(text, data))
}

fn capture(result: Result<Resource>, errors: &mut Vec<String>) -> Resource {
    match result {
        Ok(resource) => resource,
        Err(error) => {
            let reason = error.to_string();
            errors.push(reason.clone());
            Resource::new("skipped", reason)
        }
    }
}

fn protected_terminals(c: &Ctx<'_>, issue: &str, host: Option<&str>) -> Result<Vec<String>> {
    let mut stmt = c.conn.prepare(
        "SELECT c.terminal FROM claim c JOIN issue i ON i.id = c.issue_id
         WHERE c.issue_id != ?1 AND c.host = ?2 AND c.terminal IS NOT NULL
         AND (c.released_at IS NULL OR (i.state = 'waiting'
              AND c.id = (SELECT max(latest.id) FROM claim latest WHERE latest.issue_id = c.issue_id)))"
    )?;
    Ok(stmt
        .query_map((issue, host), |r| r.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}
