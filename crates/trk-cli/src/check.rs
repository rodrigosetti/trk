//! `trk check`: run the checks of waiting issues, one at a time.
//!
//! A check is a shell command an agent left behind. Exit 0 closes the issue,
//! 75 leaves it waiting, anything else returns it to open with the output as
//! a note. Only one `trk check` runs at a time, so checks that merge never
//! race each other.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use trk_core::config::parse_duration;
use trk_core::error::{Code, Error, Result};
use trk_core::model::{CHECK_RETRY_EXIT, Issue, State};
use trk_core::ops::{CheckResult, CheckRun};

use crate::env::{Env, git, hostname};
use crate::render::{Out, until};
use crate::run::pid_alive;

pub struct CheckArgs {
    pub id: Option<String>,
    pub all: bool,
    /// `Some(None)`: watch with the default pause.
    pub watch: Option<Option<String>>,
    pub dry_run: bool,
}

const WATCH_PAUSE: &str = "30s";

/// Releases the check lock when a pass ends, however it ends.
struct Lock<'a> {
    env: &'a Env,
    actor: String,
    pid: i64,
}

impl Drop for Lock<'_> {
    fn drop(&mut self) {
        if let Ok(mut s) = self.env.store() {
            let _ = s.write(&self.actor, false, |c| c.check_lock_release(self.pid));
        }
    }
}

/// Take the lock, or say who holds it. A holder whose process is gone on
/// this machine is replaced.
fn lock<'a>(env: &'a Env, actor: &str) -> Result<std::result::Result<Lock<'a>, Value>> {
    let pid = i64::from(std::process::id());
    let host = hostname();
    let mut store = env.store()?;
    let held = store.write(actor, false, |c| {
        if let Some((p, h, started)) = c.check_lock()? {
            let here = h == host;
            if p != pid && (!here || pid_alive(p)) {
                return Ok(Some(json!({"pid": p, "host": h, "started_at": started})));
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

fn kill_group(pid: u32, signal: &str) {
    let _ = Command::new("kill")
        .args([signal, "--", &format!("-{pid}")])
        .stderr(Stdio::null())
        .status();
}

/// Run `sh -c check` in its own process group, with stdout and stderr
/// captured together, for at most `timeout`.
fn run_check(
    env: &Env,
    issue: &Issue,
    check: &str,
    p: &Place,
    timeout: Duration,
) -> std::io::Result<CheckRun> {
    let dir = env.checks_dir();
    std::fs::create_dir_all(&dir)?;
    let log = dir.join(format!("{}-{}.log", issue.id, std::process::id()));
    let out = std::fs::File::create(&log)?;
    let err = out.try_clone()?;
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(check)
        .current_dir(&p.cwd)
        .stdin(Stdio::null())
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
    let mut child = cmd.spawn()?;
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break Some(st);
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            kill_group(child.id(), "-TERM");
            let grace = Instant::now();
            while grace.elapsed() < Duration::from_secs(2) {
                if child.try_wait()?.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            kill_group(child.id(), "-KILL");
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
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
fn pass(env: &Env, a: &CheckArgs) -> Result<std::result::Result<Vec<Value>, Value>> {
    let actor = env.actor()?.to_string();
    let timeout = env.check_timeout()?;
    let mut store = env.store()?;
    let queue: Vec<Issue> = match &a.id {
        Some(id) => {
            let issue = store.read(None, |c| {
                let id = c.resolve_id(id)?;
                c.issue(&id)
            })?;
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
    let _lock = match lock(env, &actor)? {
        Ok(l) => l,
        Err(holder) => return Ok(Err(holder)),
    };
    let mut out = vec![];
    for i in &queue {
        let check = i.waiting_check.clone().unwrap_or_default();
        let started = Instant::now();
        let run = match place(env, i) {
            Ok(p) => run_check(env, i, &check, &p, timeout).unwrap_or_else(|e| CheckRun {
                exit: None,
                timed_out: false,
                output: format!("[trk] could not start the check: {e}\n"),
            }),
            Err(why) => CheckRun {
                exit: None,
                timed_out: false,
                output: format!("[trk] {why}\n"),
            },
        };
        let result = store.write(&actor, false, |c| c.check_record(&i.id, &check, &run))?;
        if matches!(result, CheckResult::Passed | CheckResult::Failed) {
            let message = if result == CheckResult::Passed {
                format!(
                    "Informational update: the waiting check passed for {}. The issue is closed; no further action is required.",
                    i.id
                )
            } else {
                let reason = if run.timed_out {
                    "timed out".to_string()
                } else {
                    run.exit
                        .map(|code| format!("exited with status {code}"))
                        .unwrap_or_else(|| "could not run".to_string())
                };
                let output = run.output.trim_end();
                let context = if output.is_empty() {
                    String::new()
                } else {
                    let excerpt: String = output.chars().take(500).collect();
                    format!(" Check output: {excerpt}")
                };
                format!(
                    "Informational update: the waiting check failed for {} ({reason}); the issue was reopened. No work was resumed automatically.{context}",
                    i.id
                )
            };
            // State and outcome are already committed. UI updates are advisory:
            // an exited session or a launcher failure must not affect the check.
            let _ = crate::run::notify_waiting_session(
                env,
                &i.id,
                &message,
                result == CheckResult::Passed,
            );
        }
        let now = store.read(None, |c| c.issue(&i.id))?;
        let mut v = json!({
            "issue": i.id, "title": i.title, "result": result.as_str(),
            "exit": run.exit, "timed_out": run.timed_out,
            "duration_ms": started.elapsed().as_millis() as u64,
            "state": now.state, "next_check_at": now.next_check_at,
        });
        if result == CheckResult::Failed {
            v["output"] = json!(run.output.trim_end());
        }
        out.push(v);
    }
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
        "failed" => format!(
            "✗ {id}  {} → open, output in a note  {title}",
            if r["timed_out"].as_bool() == Some(true) {
                "timed out".to_string()
            } else {
                match r["exit"].as_i64() {
                    Some(c) => format!("failed (exit {c})"),
                    None => "could not run".to_string(),
                }
            }
        ),
        "skipped" => format!("- {id}  changed while its check ran; nothing recorded  {title}"),
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

pub fn check(env: &Env, a: CheckArgs) -> Result<Out> {
    if let Some(pause) = &a.watch {
        let pause = parse_duration(pause.as_deref().unwrap_or(WATCH_PAUSE))?;
        let pause = Duration::from_secs(pause.as_secs().max(1) as u64);
        // Runs until interrupted; each result is printed as it happens.
        loop {
            match pass(env, &a) {
                Ok(Ok(results)) => {
                    for r in &results {
                        println!("{}", line(r));
                    }
                }
                Ok(Err(holder)) => eprintln!("{}", busy_text(&holder)),
                Err(e) => eprintln!("error: {}", e.message()),
            }
            std::thread::sleep(pause);
        }
    }
    match pass(env, &a)? {
        Err(holder) => Ok(Out::Text(
            busy_text(&holder),
            json!({"busy": true, "holder": holder, "results": []}),
        )),
        Ok(results) => {
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
