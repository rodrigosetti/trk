//! `trk check`: run the checks of waiting issues, one at a time.
//!
//! A check is a shell command an agent left behind. Exit 0 closes the issue,
//! 75 leaves it waiting, anything else wakes its live session or returns it
//! to open with the output as a note. Only one `trk check` runs at a time,
//! so checks that merge never race each other.

mod restart;

use restart::Executable;
pub(crate) use restart::resume;

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
use trk_core::model::{CHECK_RETRY_EXIT, Issue, State};
use trk_core::ops::{CHECK_OUTPUT_TAIL, CheckResult, CheckRun};

use crate::env::{Env, git, hostname};
use crate::render::{Out, until};

pub struct CheckArgs {
    pub id: Option<String>,
    pub all: bool,
    /// `Some(None)`: watch with the default pause.
    pub watch: Option<Option<String>>,
    pub dry_run: bool,
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
    let held = store.write(actor, false, |c| {
        if let Some((p, h, started, pgid)) = c.check_lock()? {
            let here = host.is_some() && h == host;
            if !here || runner_alive(p) || pgid.is_some_and(group_alive) {
                return Ok(Some(json!({
                    "pid": p, "host": h, "started_at": started, "pgid": pgid,
                })));
            }
        }
        c.check_lock_take(pid, host.as_deref())?;
        Ok(None)
    })?;
    Ok(match held {
        Some(h) => Err(h),
        None => Ok(Lock {
            env,
            actor: actor.to_string(),
            pid,
            child: None,
            released: false,
        }),
    })
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

fn runner_alive(pid: i64) -> bool {
    match i32::try_from(pid) {
        Ok(pid) if pid > 0 => process_exists(pid),
        _ => true,
    }
}

fn group_alive(pgid: i64) -> bool {
    let Ok(pgid) = i32::try_from(pgid) else {
        return true;
    };
    if pgid <= 0 {
        return true;
    }
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

/// One pass over the queue. `Err(holder)` in the inner result means another
/// `trk check` is running.
fn pass(
    env: &Env,
    a: &CheckArgs,
    interrupt: &Interrupt,
    executable: &Executable,
    completed: &[Value],
) -> Result<std::result::Result<Vec<Value>, Value>> {
    let actor = env.actor()?.to_string();
    let timeout = env.check_timeout()?;
    let mut store = env.store()?;
    let queue: Vec<Issue> = match &a.id {
        Some(id) => {
            let issue = store.read(None, |c| {
                let id = c.resolve_id(id)?;
                c.issue(&id)
            })?;
            if completed.iter().any(|r| r["issue"] == issue.id) {
                return Ok(Ok(vec![]));
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
        return Ok(Ok(out));
    }
    if queue.is_empty() {
        return Ok(Ok(vec![]));
    }
    interrupt.check()?;
    let mut lock = match lock(env, &actor)? {
        Ok(l) => l,
        Err(holder) => return Ok(Err(holder)),
    };
    let mut out = vec![];
    for i in &queue {
        interrupt.check()?;
        if executable.changed()? {
            break;
        }
        let check = i.waiting_check.clone().unwrap_or_default();
        let started = Instant::now();
        let run = match place(env, i) {
            Ok(p) => match run_check(env, i, &check, &p, timeout, &mut lock, interrupt) {
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
            c.check_record(&i.id, &check, &run, session.as_ref().map(|s| s.claim))
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
            let delivered = session.as_ref().is_some_and(|s| s.send(&message).is_ok());
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
    lock.release()?;
    Ok(Ok(out))
}

fn line(r: &Value) -> String {
    let id = r["issue"].as_str().unwrap_or("");
    let title = r["title"].as_str().unwrap_or("");
    match r["result"].as_str().unwrap_or("") {
        "passed" => format!("✓ {id}  passed → closed  {title}"),
        "retry" => format!(
            "◷ {id}  not yet (exit {CHECK_RETRY_EXIT}), next {}  {title}",
            r["next_check_at"].as_str().map(until).unwrap_or_default()
        ),
        "failed" | "woken" => format!(
            "✗ {id}  {} → {}, output in a note  {title}",
            if r["timed_out"].as_bool() == Some(true) {
                "timed out".to_string()
            } else {
                match r["exit"].as_i64() {
                    Some(c) => format!("failed (exit {c})"),
                    None => "could not run".to_string(),
                }
            },
            if r["result"] == "woken" {
                "claimed, session woken"
            } else {
                "open"
            }
        ),
        "skipped" => format!("- {id}  changed while checking or delivering its wake  {title}"),
        "would_run" => {
            let mut s = format!(
                "{id}  {title}\n    check  {}",
                r["check"].as_str().unwrap_or("")
            );
            if let Some(c) = r["cwd"].as_str() {
                s.push_str(&format!("\n    in     {c}"));
            }
            if let Some(p) = r["problem"].as_str() {
                s.push_str(&format!("\n    ✗ {p}"));
            }
            s
        }
        other => format!("{id}  {other}"),
    }
}

fn busy_text(h: &Value) -> String {
    format!(
        "another `trk check` is running (pid {}{}, since {}); nothing done",
        h["pid"],
        h["host"]
            .as_str()
            .map(|x| format!(" on {x}"))
            .unwrap_or_default(),
        h["started_at"].as_str().unwrap_or("?")
    )
}

pub fn check(env: &Env, a: CheckArgs, mut completed: Vec<Value>) -> Result<Out> {
    let executable = Executable::new()?;
    let interrupt = Interrupt::new()?;
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
                Ok(Ok(results)) => {
                    for r in &results {
                        println!("{}", line(r));
                    }
                    completed.extend(results);
                }
                Ok(Err(holder)) => eprintln!("{}", busy_text(&holder)),
                Err(e) => eprintln!("error: {}", e.message()),
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
            let mut text = completed.iter().map(line).collect::<Vec<_>>();
            text.push(busy_text(&holder));
            Ok(Out::Text(
                text.join("\n"),
                json!({"busy": true, "holder": holder, "results": completed}),
            ))
        }
        Ok(results) => {
            completed.extend(results);
            if executable.changed()? {
                executable.restart(env, &completed)?;
            }
            let results = completed;
            let text = if results.is_empty() {
                if a.all || a.id.is_some() {
                    "no waiting issue has a check".to_string()
                } else {
                    "no check is due".to_string()
                }
            } else {
                results.iter().map(line).collect::<Vec<_>>().join("\n")
            };
            Ok(Out::Text(text, json!({"busy": false, "results": results})))
        }
    }
}
