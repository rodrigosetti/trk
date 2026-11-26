//! `trk check`: run the checks of waiting issues, one at a time.
//!
//! A check is a shell command an agent left behind. Exit 0 closes the issue,
//! 75 leaves it waiting, anything else wakes its live session or returns it
//! to open with the output as a note. Only one `trk check` runs at a time,
//! so checks that merge never race each other.

mod restart;

use restart::Executable;
pub(crate) use restart::resume;

use std::collections::HashSet;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use trk_core::config::parse_duration;
use trk_core::error::{Code, Error, Result};
use trk_core::model::{ActorId, CHECK_RETRY_EXIT, Issue, State};
use trk_core::ops::{CHECK_OUTPUT_TAIL, CheckResult, CheckRun};

use crate::env::{Env, git, hostname};
use crate::render::{Out, Renderer, until};

pub struct CheckArgs {
    pub id: Option<String>,
    pub all: bool,
    /// `Some(None)`: watch with the default pause.
    pub watch: Option<Option<String>>,
    pub dry_run: bool,
    pub no_gc: bool,
    pub unlock: bool,
}

const WATCH_PAUSE: &str = "30s";

/// Owns both the queue lock and its current process group. Never release the
/// durable ownership record until all live members of the group have stopped.
struct Lock<'a> {
    env: &'a Env,
    actor: String,
    pid: i64,
    child: Option<Child>,
    released: bool,
}

impl Lock<'_> {
    fn start(&mut self, cmd: &mut Command) -> std::io::Result<()> {
        self.child = Some(cmd.spawn()?);
        let child = self.child.as_mut().unwrap();
        let pgid = i64::from(child.id());
        // The shell waits on stdin. A crash before this commit closes the pipe
        // without authorizing any check side effects; afterwards recovery sees
        // the group even if this runner is killed before writing the token.
        self.env
            .store()
            .and_then(|mut s| {
                s.write(&self.actor, false, |c| {
                    c.check_lock_group(self.pid, Some(pgid))
                })
            })
            .map_err(std::io::Error::other)?;
        child.stdin.take().unwrap().write_all(b"run\n")
    }

    fn finish(&mut self) -> std::io::Result<()> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        let pgid = child.id();
        kill_group(pgid, libc::SIGTERM);
        let grace = Instant::now();
        loop {
            child.try_wait()?;
            if !group_alive(i64::from(pgid)) {
                break;
            }
            if grace.elapsed() >= Duration::from_secs(2) {
                kill_group(pgid, libc::SIGKILL);
            }
            // If cleanup cannot establish safety, leave the durable lock for
            // recovery instead of ever starting another check.
            if grace.elapsed() >= Duration::from_secs(5) {
                return Err(std::io::Error::other("check process group did not stop"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        child.wait()?;
        self.env
            .store()
            .and_then(|mut s| s.write(&self.actor, false, |c| c.check_lock_group(self.pid, None)))
            .map_err(std::io::Error::other)?;
        self.child = None;
        Ok(())
    }

    fn release(&mut self) -> Result<()> {
        self.finish()?;
        self.env
            .store()?
            .write(&self.actor, false, |c| c.check_lock_release(self.pid))?;
        self.released = true;
        Ok(())
    }
}

impl Drop for Lock<'_> {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.release();
        }
    }
}

/// Take the lock only when neither the previous runner nor its group survives.
/// Unknown hosts or liveness failures conservatively retain ownership.
fn lock<'a>(env: &'a Env, actor: &str) -> Result<std::result::Result<Lock<'a>, Value>> {
    let pid = i64::from(std::process::id());
    let host = hostname();
    let mut store = env.store()?;
    loop {
        let previous = store.read(None, |c| c.check_lock())?;
        // Liveness probes may run ps. Never hold the SQLite writer while
        // waiting for an external command, even during stale-lock recovery.
        if let Some((p, h, started, pgid)) = &previous {
            let here = host.is_some() && *h == host;
            if !here || runner_alive(*p, started) || pgid.is_some_and(group_alive) {
                return Ok(Err(json!({
                    "pid": p, "host": h, "started_at": started, "pgid": pgid,
                })));
            }
        }
        let taken = store.write(actor, false, |c| {
            if c.check_lock()? != previous {
                return Ok(false);
            }
            c.check_lock_take(pid, host.as_deref())?;
            Ok(true)
        })?;
        if taken {
            return Ok(Ok(Lock {
                env,
                actor: actor.to_string(),
                pid,
                child: None,
                released: false,
            }));
        }
    }
}

/// Human resume serializes restoration and delivery with check subprocesses.
pub(crate) fn with_queue_lock<T>(env: &Env, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let actor = env.actor()?.to_string();
    let mut lock = lock(env, &actor)?.map_err(|holder| {
        Error::new(Code::InvalidState, "check queue is busy; resume refused")
            .with_detail("holder", holder)
            .with_hint("wait for the in-flight check and retry `trk resume`; for a stale holder, ask a human to inspect it and run `trk check --unlock`")
    })?;
    let result = f()?;
    lock.release()?;
    Ok(result)
}

/// Serialize GC with checks, not with unrelated database writers. Previews
/// inspect ownership without persisting a lock or keeping a transaction open.
pub(crate) fn with_gc_lock<T>(
    env: &Env,
    dry_run: bool,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let actor = env.actor()?.to_string();
    let busy = |holder| {
        Error::new(Code::InvalidState, "check queue is busy; gc refused")
            .with_detail("holder", holder)
            .with_hint("wait for the in-flight check and retry `trk gc`; for a stale holder, ask a human to inspect it and run `trk check --unlock`")
    };
    if dry_run {
        let host = hostname();
        let previous = env.store()?.read(None, |c| c.check_lock())?;
        if let Some((pid, holder_host, started, pgid)) = previous
            && (host.is_none()
                || holder_host != host
                || runner_alive(pid, &started)
                || pgid.is_some_and(group_alive))
        {
            return Err(busy(json!({
                "pid": pid, "host": holder_host, "started_at": started, "pgid": pgid,
            })));
        }
        return f();
    }
    let mut lock = lock(env, &actor)?.map_err(busy)?;
    let result = f()?;
    lock.release()?;
    Ok(result)
}

/// Where a check runs and what it sees.
struct Place {
    cwd: PathBuf,
    repo: Option<PathBuf>,
    branch: String,
}

fn place(env: &Env, issue: &Issue) -> std::result::Result<Place, String> {
    let repo = env.checkout();
    let cwd = match &issue.wait_cwd {
        Some(w) if Path::new(w).is_dir() => PathBuf::from(w),
        Some(w) => {
            return Err(format!(
                "the check's directory {w} no longer exists, so the check cannot run"
            ));
        }
        None => match &repo {
            Some(r) => r.clone(),
            None => env.root.clone(),
        },
    };
    let branch = git(&cwd, &["branch", "--show-current"])
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| issue.id.clone());
    Ok(Place { cwd, repo, branch })
}

fn kill_group(pgid: u32, signal: i32) {
    // SAFETY: kill takes scalar arguments; a negative PID addresses the group.
    unsafe { libc::kill(-(pgid as i32), signal) };
}

fn process_exists(pid: i32) -> bool {
    // SAFETY: signal 0 probes existence without delivering a signal. Errors
    // other than ESRCH (including permission errors) cannot establish safety.
    (unsafe { libc::kill(pid, 0) }) == 0
        || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

fn runner_alive(pid: i64, started: &str) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return true;
    };
    if pid <= 0 {
        return true;
    }
    if !process_exists(pid) {
        return false;
    }
    // ps elapsed time is portable across macOS/Linux and avoids local-time
    // ambiguity in lstart. Sample before ps and subtract another second for its
    // rounding: only a process definitely newer than the lock proves PID reuse.
    let observed = jiff::Timestamp::now().as_second();
    let reused = || -> Option<bool> {
        let locked = started.parse::<jiff::Timestamp>().ok()?.as_second();
        let out = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "etime="])
            .env("LC_ALL", "C")
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = std::str::from_utf8(&out.stdout).ok()?.trim();
        let (days, clock) = match text.split_once('-') {
            Some((days, clock)) => (days.parse::<i64>().ok()?, clock),
            None => (0, text),
        };
        let mut fields = clock.rsplit(':');
        let seconds = fields.next()?.parse::<i64>().ok()?;
        let minutes = fields.next()?.parse::<i64>().ok()?;
        let hours = fields
            .next()
            .map(str::parse::<i64>)
            .transpose()
            .ok()?
            .unwrap_or(0);
        if fields.next().is_some()
            || days < 0
            || hours < 0
            || !(0..60).contains(&minutes)
            || !(0..60).contains(&seconds)
        {
            return None;
        }
        let elapsed = days
            .checked_mul(86400)?
            .checked_add(hours.checked_mul(3600)?)?
            .checked_add(minutes * 60 + seconds)?;
        Some(observed.checked_sub(elapsed)?.checked_sub(1)? > locked)
    };
    !reused().unwrap_or(false)
}

fn group_alive(pgid: i64) -> bool {
    let Ok(pgid) = i32::try_from(pgid) else {
        return true;
    };
    if !process_exists(-pgid) {
        return false;
    }
    // Orphaned zombies cannot execute side effects, but kill(0) still sees
    // them on systems whose init does not promptly reap adopted descendants.
    let Ok(out) = Command::new("ps").args(["-axo", "pgid=,stat="]).output() else {
        return true;
    };
    if !out.status.success() {
        return true;
    }
    String::from_utf8_lossy(&out.stdout).lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next().and_then(|s| s.parse::<i32>().ok()) == Some(pgid)
            && !fields.next().is_some_and(|s| s.starts_with('Z'))
    })
}

/// Signal handlers only set a flag; ordinary control flow owns all cleanup.
struct Interrupt {
    signal: Arc<AtomicUsize>,
    handlers: Vec<signal_hook::SigId>,
}

impl Interrupt {
    fn new() -> std::io::Result<Self> {
        let mut interrupt = Self {
            signal: Arc::new(AtomicUsize::new(0)),
            handlers: Vec::with_capacity(3),
        };
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            interrupt.handlers.push(signal_hook::flag::register_usize(
                signal,
                Arc::clone(&interrupt.signal),
                signal as usize,
            )?);
        }
        Ok(interrupt)
    }

    fn check(&self) -> std::io::Result<()> {
        match self.signal.load(Ordering::Relaxed) {
            0 => Ok(()),
            signal => Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                format!("check runner interrupted by signal {signal}"),
            )),
        }
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        for handler in self.handlers.drain(..) {
            signal_hook::low_level::unregister(handler);
        }
    }
}

/// Run `sh -c check` in its own process group, with stdout and stderr
/// captured together, for at most `timeout`.
fn run_check(
    env: &Env,
    issue: &Issue,
    check: &str,
    p: &Place,
    timeout: Duration,
    lock: &mut Lock<'_>,
    interrupt: &Interrupt,
) -> std::io::Result<CheckRun> {
    let dir = env.checks_dir();
    std::fs::create_dir_all(&dir)?;
    let log = dir.join(format!("{}-{}.log", issue.id, std::process::id()));
    let out = std::fs::File::create(&log)?;
    let err = out.try_clone()?;
    let mut cmd = Command::new("sh");
    // The gate prevents a crash between spawn and durable group registration
    // from leaving an untracked command running. EOF means do not execute.
    cmd.args([
        "-c",
        "IFS= read -r start || exit 125; [ \"$start\" = run ] || exit 125; exec sh -c \"$1\"",
        "trk-check",
        check,
    ])
    .current_dir(&p.cwd)
    .stdin(Stdio::piped())
    .stdout(out)
    .stderr(err)
    .env("TRK_ISSUE", &issue.id)
    .env("TRK_WORKTREE", &p.cwd)
    .env("TRK_BRANCH", &p.branch)
    .env("TRK_BASE", env.base())
    .env("TRK_VERIFY", &env.user.check.verify)
    .env("TRK_DIR", env.dir_env())
    // The check is not an agent: it must not inherit a session's identity.
    .env_remove("TRK_ACTOR")
    .process_group(0);
    match &p.repo {
        Some(r) => cmd.env("TRK_REPO", r),
        None => cmd.env_remove("TRK_REPO"),
    };
    interrupt.check()?;
    lock.start(&mut cmd)?;
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        interrupt.check()?;
        if let Some(st) = lock.child.as_mut().unwrap().try_wait()? {
            break Some(st);
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // Also stop background descendants after a successful shell exit.
    lock.finish()?;
    interrupt.check()?;
    let mut output = String::from_utf8_lossy(&std::fs::read(&log)?).to_string();
    let _ = std::fs::remove_file(&log);
    if timed_out {
        output.push_str(&format!(
            "\n[trk] killed after {}s: the check timeout ([check] timeout in config.toml)\n",
            timeout.as_secs()
        ));
    }
    Ok(CheckRun {
        exit: status.and_then(|s| s.code()),
        timed_out,
        output,
    })
}

#[derive(Default)]
struct Pass {
    results: Vec<Value>,
    gc: Option<Value>,
}

/// A sweep is advisory and separate from recording check outcomes. Real passes
/// already own the queue lock; previews only inspect queue ownership.
fn sweep(env: &Env, a: &CheckArgs) -> Option<Value> {
    if a.id.is_some() || a.no_gc || !env.user.check.gc {
        return None;
    }
    let collect = || {
        crate::gc::collect(
            env,
            &[],
            a.dry_run,
            false,
            Some(&env.user.check.gc_grace),
            true,
        )
    };
    let result = if a.dry_run {
        with_gc_lock(env, true, collect)
    } else {
        collect()
    };
    let data = result.unwrap_or_else(
        |error| json!({"dry_run": a.dry_run, "issues": [], "errors": [error.to_string()]}),
    );
    if data["issues"].as_array().is_some_and(|v| !v.is_empty())
        || data["errors"].as_array().is_some_and(|v| !v.is_empty())
    {
        Some(data)
    } else {
        None
    }
}

fn gc_lines(gc: Option<&Value>, seen_skips: &mut HashSet<String>, r: &Renderer) -> Vec<String> {
    let mut lines = Vec::new();
    let Some(gc) = gc else { return lines };
    if let Some(issues) = gc["issues"].as_array() {
        for issue in issues {
            let id = issue["id"].as_str().unwrap_or_default();
            let removed: Vec<_> = ["session", "worktree", "branch"]
                .into_iter()
                .filter(|resource| issue[*resource]["status"] == "removed")
                .collect();
            if !removed.is_empty() {
                lines.push(format!(
                    "{}  {} {}",
                    r.issue_id(id),
                    if gc["dry_run"] == true {
                        r.activity("gc: would remove")
                    } else {
                        r.ready("gc: removed")
                    },
                    removed.join(", "),
                ));
            }
            if !seen_skips.contains(id) {
                let reasons = ["session", "worktree", "branch"]
                    .into_iter()
                    .filter(|resource| issue[*resource]["status"] == "skipped")
                    .map(|resource| {
                        format!(
                            "{resource}: {}",
                            issue[resource]["reason"].as_str().unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                if !reasons.is_empty() {
                    seen_skips.insert(id.to_owned());
                    if removed.is_empty() {
                        lines.push(format!(
                            "{}  {} {reasons}",
                            r.issue_id(id),
                            r.warning("gc: skipped")
                        ));
                    } else {
                        lines
                            .last_mut()
                            .unwrap()
                            .push_str(&format!("; {} {reasons}", r.warning("skipped")));
                    }
                }
            }
        }
    }
    if let Some(errors) = gc["errors"].as_array() {
        for error in errors.iter().filter_map(Value::as_str) {
            lines.push(r.warning(&format!("gc: {error} (will retry on the next pass)")));
        }
    }
    lines
}

/// One pass over the queue. `Err(holder)` in the inner result means another
/// `trk check` is running.
fn pass(
    env: &Env,
    a: &CheckArgs,
    interrupt: &Interrupt,
    executable: &Executable,
    completed: &[Value],
) -> Result<std::result::Result<Pass, Value>> {
    let actor = env.actor()?.to_string();
    let timeout = env.check_timeout()?;
    let mut store = env.store()?;
    // Snapshot the queue only after ownership: a resume may have completed
    // between an earlier snapshot and lock acquisition.
    let mut queue_lock = if a.dry_run {
        None
    } else {
        interrupt.check()?;
        match lock(env, &actor)? {
            Ok(l) => Some(l),
            Err(holder) => return Ok(Err(holder)),
        }
    };
    let queue: Vec<Issue> = match &a.id {
        Some(id) => {
            let issue = store.read(None, |c| {
                let id = c.resolve_id(id)?;
                c.issue(&id)
            })?;
            if completed.iter().any(|r| r["issue"] == issue.id) {
                return Ok(Ok(Pass::default()));
            }
            if issue.state != State::Waiting || issue.waiting_check.is_none() {
                return Err(Error::new(
                    Code::InvalidState,
                    format!(
                        "{} is {}{}; there is no check to run",
                        issue.id,
                        issue.state,
                        if issue.state == State::Waiting {
                            " on a human"
                        } else {
                            ""
                        }
                    ),
                )
                .with_issue(&issue.id));
            }
            vec![issue]
        }
        None => store.read(None, |c| c.check_queue(a.all))?,
    };
    let queue: Vec<_> = queue
        .into_iter()
        .filter(|i| !completed.iter().any(|r| r["issue"] == i.id))
        .collect();
    if a.dry_run {
        let mut out = vec![];
        for i in &queue {
            let p = place(env, i);
            out.push(json!({
                "issue": i.id, "title": i.title, "result": "would_run",
                "check": i.waiting_check, "reason": i.waiting_reason,
                "cwd": p.as_ref().ok().map(|p| p.cwd.clone()),
                "problem": p.err(),
            }));
        }
        return Ok(Ok(Pass {
            results: out,
            gc: sweep(env, a),
        }));
    }
    interrupt.check()?;
    let lock = queue_lock.as_mut().unwrap();
    let mut out = vec![];
    for i in &queue {
        interrupt.check()?;
        if executable.changed()? {
            break;
        }
        let (current, revision) =
            store.read(None, |c| Ok((c.issue(&i.id)?, c.issue_revision(&i.id)?)))?;
        if current.state != State::Waiting || current.waiting_check.is_none() {
            continue;
        }
        let i = &current;
        let check = i.waiting_check.clone().unwrap_or_default();
        let started = Instant::now();
        let run = match place(env, i) {
            Ok(p) => match run_check(env, i, &check, &p, timeout, lock, interrupt) {
                Ok(run) => run,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted || lock.child.is_some() => {
                    return Err(e.into());
                }
                Err(e) => CheckRun {
                    exit: None,
                    timed_out: false,
                    output: format!("[trk] could not start the check: {e}\n"),
                },
            },
            Err(why) => CheckRun {
                exit: None,
                timed_out: false,
                output: format!("[trk] {why}\n"),
            },
        };
        interrupt.check()?;
        let session = if (run.timed_out || !matches!(run.exit, Some(0 | CHECK_RETRY_EXIT)))
            && i.wake_count < env.user.check.max_wakes
        {
            // Unknown liveness is not enough to restore a claim.
            crate::run::waiting_session(env, &i.id).unwrap_or(None)
        } else {
            None
        };
        let mut result = store.write(&actor, false, |c| {
            c.check_record(
                &i.id,
                &check,
                revision,
                &run,
                session.as_ref().map(|s| s.claim),
            )
        })?;
        if let CheckResult::Woken { claim } = result {
            let wake_count = store.read(None, |c| Ok(c.issue(&i.id)?.wake_count))?;
            let reason = if run.timed_out {
                "timed out".to_string()
            } else {
                run.exit
                    .map(|code| format!("exited with status {code}"))
                    .unwrap_or_else(|| "could not run".to_string())
            };
            let output = run.output.trim_end();
            let mut start = output.len().saturating_sub(CHECK_OUTPUT_TAIL);
            while !output.is_char_boundary(start) {
                start += 1;
            }
            let message = format!(
                "The waiting check failed for {} ({reason}; wake {wake_count} of {}). Your claim has been restored. \
                 Address the failure using the check output below and the issue's notes. \
                 Then end the session again, normally by calling wait with a corrected check, \
                 or hand_back if the failure cannot be fixed by this session.\
                 \n\nCheck:\n{check}\n\nOutput:\n{}",
                i.id,
                env.user.check.max_wakes,
                if output.is_empty() {
                    "(no output)"
                } else {
                    &output[start..]
                },
            );
            let delivered = session
                .as_ref()
                .is_some_and(|s| s.send("check-wake", &actor, &message).is_ok());
            if !delivered {
                result = if store.write(&actor, false, |c| c.check_wake_failed(&i.id, claim))? {
                    CheckResult::Failed
                } else {
                    CheckResult::Skipped
                };
            }
        } else if result == CheckResult::Passed {
            // Passing UI updates remain advisory, including the Herdr marker.
            let message = format!(
                "Informational update: the waiting check passed for {}. The issue is closed; no further action is required.",
                i.id
            );
            let _ = crate::run::notify_waiting_session(env, &i.id, &message, true);
        }
        let now = store.read(None, |c| c.issue(&i.id))?;
        let mut v = json!({
            "issue": i.id, "title": i.title, "result": result.as_str(),
            "exit": run.exit, "timed_out": run.timed_out,
            "duration_ms": started.elapsed().as_millis() as u64,
            "state": now.state, "next_check_at": now.next_check_at,
        });
        if matches!(result, CheckResult::Failed | CheckResult::Woken { .. }) {
            v["output"] = json!(run.output.trim_end());
        }
        out.push(v);
    }
    let gc = sweep(env, a);
    lock.release()?;
    Ok(Ok(Pass { results: out, gc }))
}

fn line(v: &Value, r: &Renderer) -> String {
    let id = r.issue_id(v["issue"].as_str().unwrap_or(""));
    let title = v["title"].as_str().unwrap_or("");
    match v["result"].as_str().unwrap_or("") {
        "passed" => format!(
            "{} {id}  {}  {title}",
            r.ready("✓"),
            r.ready("passed → closed")
        ),
        "retry" => format!(
            "{} {id}  {}  {title}",
            r.activity("◷"),
            r.activity(&format!(
                "not yet (exit {CHECK_RETRY_EXIT}), next {}",
                v["next_check_at"].as_str().map(until).unwrap_or_default()
            ))
        ),
        "failed" | "woken" => format!(
            "{} {id}  {} → {}, output in a note  {title}",
            r.failure("✗"),
            r.failure(&if v["timed_out"].as_bool() == Some(true) {
                "timed out".to_string()
            } else {
                match v["exit"].as_i64() {
                    Some(c) => format!("failed (exit {c})"),
                    None => "could not run".to_string(),
                }
            }),
            if v["result"] == "woken" {
                "claimed, session woken"
            } else {
                "open"
            }
        ),
        "skipped" => format!(
            "{} {id}  {}  {title}",
            r.warning("-"),
            r.warning("changed while checking or delivering its wake")
        ),
        "would_run" => {
            let mut s = format!(
                "{id}  {title}\n    {}  {}",
                r.activity("check"),
                v["check"].as_str().unwrap_or("")
            );
            if let Some(c) = v["cwd"].as_str() {
                s.push_str(&format!("\n    {}     {c}", r.activity("in")));
            }
            if let Some(p) = v["problem"].as_str() {
                s.push_str(&format!("\n    {} {p}", r.failure("✗")));
            }
            s
        }
        other => format!("{id}  {other}"),
    }
}

fn busy_text(h: &Value) -> String {
    format!(
        "check queue is busy (pid {}{}, since {}); nothing done. For a stale holder, ask a human to inspect it and run `trk check --unlock`",
        h["pid"],
        h["host"]
            .as_str()
            .map(|x| format!(" on {x}"))
            .unwrap_or_default(),
        h["started_at"].as_str().unwrap_or("?")
    )
}

fn unlock(env: &Env) -> Result<Out> {
    let actor = env.actor()?;
    if ActorId::parse(actor)?.is_agent() {
        return Err(Error::new(
            Code::NotPermitted,
            "only a human may unlock the check queue",
        )
        .with_hint("ask the human to inspect the holder and run `trk check --unlock`"));
    }
    let mut store = env.store()?;
    let holder = store.read(None, |c| c.check_lock())?;
    if let Some((pid, host, started, pgid)) = &holder
        && hostname().is_some_and(|here| host.as_ref() == Some(&here))
        && (runner_alive(*pid, started) || pgid.is_some_and(group_alive))
    {
        return Err(Error::new(
            Code::InvalidState,
            "check queue holder may still be running; unlock refused",
        )
        .with_detail(
            "holder",
            json!({
                "pid": pid, "host": host, "started_at": started, "pgid": pgid,
            }),
        )
        .with_hint("stop the runner and its check process group before unlocking"));
    }
    let unlocked = store.write(actor, false, |c| c.check_lock_unlock(holder.as_ref()))?;
    let holder = holder.map(|(pid, host, started, pgid)| {
        json!({
            "pid": pid, "host": host, "started_at": started, "pgid": pgid,
        })
    });
    let text = match &holder {
        Some(h) => format!(
            "unlocked check queue (pid {}, host {}, since {}, pgid {})",
            h["pid"], h["host"], h["started_at"], h["pgid"],
        ),
        None => "check queue is not locked; nothing done".to_string(),
    };
    Ok(Out::Text(
        text,
        json!({"unlocked": unlocked, "holder": holder}),
    ))
}

pub fn check(
    env: &Env,
    a: CheckArgs,
    mut completed: Vec<Value>,
    renderer: &Renderer,
) -> Result<Out> {
    if a.unlock {
        return unlock(env);
    }
    let executable = Executable::new()?;
    let interrupt = Interrupt::new()?;
    let mut seen_skips = HashSet::new();
    if let Some(pause) = &a.watch {
        let pause = parse_duration(pause.as_deref().unwrap_or(WATCH_PAUSE))?;
        let pause = Duration::from_secs(pause.as_secs().max(1) as u64);
        // Runs until interrupted; each result is printed as it happens.
        loop {
            interrupt.check()?;
            if executable.changed()? {
                executable.restart(env, &completed)?;
            }
            match pass(env, &a, &interrupt, &executable, &completed) {
                Ok(Ok(pass)) => {
                    for r in &pass.results {
                        println!("{}", line(r, renderer));
                    }
                    for line in gc_lines(pass.gc.as_ref(), &mut seen_skips, renderer) {
                        println!("{line}");
                    }
                    completed.extend(pass.results);
                }
                Ok(Err(holder)) => eprintln!("{}", renderer.warning(&busy_text(&holder))),
                Err(e) => eprintln!("{} {}", renderer.failure("error:"), e.message()),
            }
            let until = Instant::now() + pause;
            while Instant::now() < until {
                interrupt.check()?;
                if executable.changed()? {
                    executable.restart(env, &completed)?;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            completed.clear();
        }
    }
    if executable.changed()? {
        executable.restart(env, &completed)?;
    }
    match pass(env, &a, &interrupt, &executable, &completed)? {
        Err(holder) => {
            let mut text = completed
                .iter()
                .map(|v| line(v, renderer))
                .collect::<Vec<_>>();
            text.push(renderer.warning(&busy_text(&holder)));
            Ok(Out::Text(
                text.join("\n"),
                json!({"busy": true, "holder": holder, "results": completed}),
            ))
        }
        Ok(pass) => {
            completed.extend(pass.results);
            if executable.changed()? {
                executable.restart(env, &completed)?;
            }
            let results = completed;
            let mut text = if results.is_empty() {
                if a.all || a.id.is_some() {
                    renderer.activity("no waiting issue has a check")
                } else {
                    renderer.activity("no check is due")
                }
            } else {
                results
                    .iter()
                    .map(|v| line(v, renderer))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let gc_text = gc_lines(pass.gc.as_ref(), &mut seen_skips, renderer);
            if !gc_text.is_empty() {
                text.push('\n');
                text.push_str(&gc_text.join("\n"));
            }
            let mut data = json!({"busy": false, "results": results});
            if let Some(gc) = pass.gc {
                data["gc"] = gc;
            }
            Ok(Out::Text(text, data))
        }
    }
}
