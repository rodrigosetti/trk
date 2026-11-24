//! `trk run` and friends: launcher profiles and persistent sessions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use trk_agent::harness::{self, LaunchInput};
use trk_core::error::{Code, Error, Result};
use trk_core::model::{ActorId, Claim, State};
use trk_core::ops::ClaimOpts;
use trk_core::view::With;

use crate::env::{Env, git, hostname};
use crate::render::Out;

use crate::launcher::{self, Launcher, run_template};

/// A session that is still starting may refuse input.
fn send_with_retry(
    tmpl: &[String],
    vars: &BTreeMap<&str, String>,
    retries: Option<u64>,
    retry_delay: Option<u64>,
) -> Result<(bool, String)> {
    let mut last = (false, String::new());
    let delay = std::time::Duration::from_secs(retry_delay.unwrap_or(3));
    for attempt in 0..retries.unwrap_or(10).max(1) {
        if attempt > 0 {
            std::thread::sleep(delay);
        }
        last = run_template(tmpl, vars, false)?;
        if last.0 {
            break;
        }
    }
    Ok(last)
}

pub struct RunArgs {
    pub id: Option<String>,
    pub agent: Option<String>,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub fg: bool,
    pub print: bool,
    pub dry_run: bool,
}

/// Terminal names: 1-32 of a-z 0-9 - _.
fn terminal_name(id: &str) -> String {
    let s: String = format!("trk-{id}")
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    s.chars().take(32).collect()
}

fn ensure_worktree(checkout: &Path, path: &Path, branch: &str, base: &str) -> Result<()> {
    if path.join(".git").exists() {
        return Ok(());
    }
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let exists = git(
        checkout,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_some();
    let p = path.to_string_lossy().to_string();
    let args: Vec<&str> = if exists {
        vec!["worktree", "add", &p, branch]
    } else {
        vec!["worktree", "add", &p, "-b", branch, base]
    };
    let out = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(&args)
        .output()?;
    if !out.status.success() {
        return Err(Error::new(
            Code::Internal,
            format!(
                "git worktree add failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    Ok(())
}

fn trk_bin() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "trk".into())
}

pub fn run(env: &Env, a: RunArgs) -> Result<Out> {
    let human = env.actor()?.to_string();
    let mut store = env.store()?;
    let issue = if let Some(id) = a.id.as_deref() {
        store.read(Some(&human), |c| {
            let id = c.resolve_id(id)?;
            c.show(
                &id,
                With {
                    deps: true,
                    ..Default::default()
                },
            )
        })?
    } else {
        store
            .read(Some(&human), |c| c.list("ready", None, Some(1)))?
            .into_iter()
            .next()
            .ok_or_else(|| {
                Error::new(Code::NotFound, "no ready issues").with_hint(
                    "use `trk ready` to inspect ready work, or pass an explicit issue ID",
                )
            })?
    };
    let id = issue["id"].as_str().unwrap_or_default().to_string();

    // 1. Ready, agent and harness.
    if issue["derived"]["ready"].as_bool() != Some(true) {
        if let Some(c) = issue["claim"]["actor"].as_str() {
            return Err(
                Error::new(Code::ClaimedByOther, format!("{id} is claimed by {c}"))
                    .with_issue(&id)
                    .with_hint(format!("see `trk runs`; `trk attach {id}` to take over")),
            );
        }
        if let Some(b) = issue["derived"]["blocked_by"]
            .as_array()
            .filter(|b| !b.is_empty())
        {
            return Err(Error::new(Code::Blocked, format!("{id} is blocked"))
                .with_issue(&id)
                .with_detail("blocked_by", Value::Array(b.clone())));
        }
        let state = issue["state"].as_str().unwrap_or("");
        let mut e =
            Error::new(Code::InvalidState, format!("{id} is {state}, not ready")).with_issue(&id);
        if state != "open" {
            e = e.with_hint(format!("trk reopen {id}"));
        }
        return Err(e);
    }
    let routing_config = trk_agent::routing::RoutingConfig::new(&env.config);
    let routing = routing_config.resolve(
        issue["type"].as_str().unwrap_or_default(),
        a.agent.as_deref(),
    )?;
    let resolved = routing_config.load(&routing)?;
    let agent = resolved.agent;
    let agent_name = agent.name.clone();
    let profiles = harness::profiles(&env.user)?;
    let hname = harness::choose(&agent, &profiles, a.harness.as_deref())?;
    let profile = profiles[&hname].clone();

    let selected = if a.fg || a.print || a.dry_run {
        None
    } else {
        Some(launcher::choose(env)?)
    };
    // 2. Worktree.
    let checkout = env.checkout();
    let base = env.base();
    let worktree = checkout
        .as_ref()
        .filter(|_| routing.worktree)
        .map(|_| env.worktrees_dir().join(&id));

    // 3. Actor.
    let uuid = uuid::Uuid::new_v4().to_string();
    let actor = format!("agent:{hname}:{uuid}");
    let mut terminal = terminal_name(&id);
    let session_dir = env.sessions_dir().join(&uuid);
    let cwd = if routing.worktree {
        worktree.as_ref().unwrap_or(&session_dir)
    } else {
        &env.root
    };

    let mut warnings: Vec<String> = vec![];
    if !a.dry_run {
        if let (Some(co), Some(wt)) = (&checkout, &worktree) {
            // The branch is always the issue id: a later session finds the same one.
            ensure_worktree(co, wt, &id, &base)?;
            // Agents reach trk through MCP only; a committed skill would reach them anyway.
            for rel in [".agents/skills/trk", ".claude/skills/trk"] {
                if wt.join(rel).exists() {
                    let w = format!(
                        "{rel} is tracked in this repo, so the session will see the trk CLI skill"
                    );
                    eprintln!("warning: {w}");
                    warnings.push(w);
                }
            }
        }
        std::fs::create_dir_all(&session_dir)?;
    }

    // 4. Claim.
    let wt_str = worktree.as_ref().map(|w| w.to_string_lossy().to_string());
    let claim_opts = ClaimOpts {
        lease: None,
        worktree: wt_str.clone(),
        host: hostname(),
        agent: Some(agent_name.clone()),
        terminal: if a.fg { None } else { Some(terminal.clone()) },
        launcher: selected.as_ref().and_then(|(l, _)| l.name.clone()),
        steal: false,
        on_behalf_of: Some(human.clone()),
    };
    // In a dry run the claim is rolled back, but the issue is rendered as the
    // agent would see it after claiming.
    let tools = trk_agent::agent_tools(&agent, &env.user.check);
    let issue_now = store.write(&actor, a.dry_run, |c| {
        c.claim(&id, claim_opts)?;
        // Notes go in the briefing: they are how a returning issue explains itself.
        c.show(
            &id,
            With {
                deps: true,
                notes: true,
                ..Default::default()
            },
        )
    })?;

    // 5. Render the launch spec.
    let input = trk_agent::prompt::PromptInput {
        issue: &issue_now,
        project_root: &env.root,
        config: &env.user,
        issue_type: &routing.issue_type,
        type_description: &routing.description,
        agent: &agent,
        tools: &tools,
        worktree: wt_str.as_deref(),
        actor: &actor,
        harness: Some(&hname),
        base: &base,
        verify: &env.user.check.verify,
    };
    let instructions = trk_agent::prompt::instructions(&resolved.config, &input)?;
    let fold = if profile.server_instructions {
        None
    } else {
        Some(instructions.as_str())
    };
    let briefing = trk_agent::prompt::briefing(&resolved.config, &input, fold)?;
    let mut trk_env = BTreeMap::new();
    trk_env.insert("TRK_DIR".to_string(), env.dir_env());
    let cwd_s = cwd.to_string_lossy().to_string();
    let spec = harness::build(LaunchInput {
        issue: &id,
        agent: &agent,
        harness: &hname,
        profile: &profile,
        model: a.model.as_deref(),
        actor: &actor,
        on_behalf_of: Some(&human),
        terminal: &terminal,
        cwd: &cwd_s,
        worktree: wt_str.as_deref(),
        session_dir: &session_dir.to_string_lossy(),
        trk_bin: &trk_bin(),
        trk_env,
        briefing,
        instructions,
        tools: tools.iter().map(|t| t.name.clone()).collect(),
    })?;
    let mut spec_json = serde_json::to_value(&spec)?;
    spec_json["type"] = json!(routing.issue_type);
    spec_json["routing"] = json!(routing);
    if a.dry_run {
        return Ok(Out::Data(spec_json));
    }

    // Files the harness reads.
    for (path, content) in &spec.files {
        let p = PathBuf::from(path);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, content)?;
        if checkout.is_some()
            && let Ok(rel) = p.strip_prefix(cwd)
        {
            crate::env::git_exclude(cwd, &format!("/{}", rel.to_string_lossy()));
        }
    }
    if a.print {
        return Ok(Out::Data(spec_json));
    }

    // 6. Start.
    let undo = |store: &mut trk_core::Store, why: &str| {
        let _ = store.write(&actor, false, |c| c.unclaim(&id, false, Some(why)));
    };
    if a.fg {
        let st = Command::new("sh")
            .arg("-c")
            .arg(&spec.shell_command)
            .status()?;
        return Ok(Out::Text(
            format!(
                "{} exited ({}). Its claim stays until the lease expires or `trk reap`.",
                hname, st
            ),
            json!({"issue": id, "actor": actor, "exit_code": st.code()}),
        ));
    }
    let (l, fallback_placement) = selected.expect("background launch selects a profile");
    let mut vars = BTreeMap::new();
    vars.insert("name", terminal.clone());
    vars.insert("cwd", cwd_s.clone());
    vars.insert("shell_command", spec.shell_command.clone());
    let title = issue["title"].as_str().unwrap_or(&id);
    vars.insert("title", title.to_owned());
    let (output, placement) = if l.is_herdr() {
        let (address, placement) = crate::herdr::start(title, &cwd_s, &spec.shell_command)
            .map_err(|reason| {
                Error::new(Code::Internal, format!("Herdr launch unconfirmed: {reason}"))
                    .with_issue(&id)
                    .with_hint("claim retained to avoid a duplicate launch; inspect the launcher before unclaiming")
            })?;
        store.write(&actor, false, |c| c.bind_terminal(&id, &address))?;
        terminal = address;
        vars.insert("name", terminal.clone());
        (String::new(), Some(placement))
    } else {
        let Some(start) = l.start.as_deref().filter(|start| !start.is_empty()) else {
            undo(&mut store, "launch failed: no launcher start template");
            return Err(Error::new(
                Code::ConfigInvalid,
                "no launcher start template in config.toml",
            )
            .with_hint("use --fg to run in this terminal, or configure [launcher]"));
        };
        let out = match launcher::capture(start, &vars) {
            Ok(out) if out.success => out,
            result => {
                let reason = match result {
                    Ok(out) => out.text,
                    Err(error) => error.to_string(),
                };
                undo(&mut store, &format!("launch failed: {reason}"));
                return Err(
                    Error::new(Code::Internal, format!("launcher start failed: {reason}"))
                        .with_issue(&id),
                );
            }
        };
        if let Some(address) = out
            .stdout
            .lines()
            .next()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            store.write(&actor, false, |c| c.bind_terminal(&id, address))?;
            terminal = address.to_owned();
            vars.insert("name", terminal.clone());
        }
        (out.text, fallback_placement)
    };
    if let Some(warning) = placement.as_ref().and_then(|p| p.warning()) {
        eprintln!("warning: {warning}");
        warnings.push(warning);
    }
    let mut sent = false;
    if spec.send_briefing
        && let Some(send) = &l.send
    {
        std::thread::sleep(std::time::Duration::from_secs(l.send_delay.unwrap_or(5)));
        vars.insert("text", spec.briefing.clone());
        let (ok, out) = send_with_retry(send, &vars, l.send_retries, l.send_retry_delay)?;
        if !ok {
            eprintln!("warning: sending the briefing failed: {out}");
        }
        sent = ok;
    }
    let mut text = format!(
        "Started {agent_name} ({hname}) on {id} in terminal {terminal}\n  actor    {actor}\n  cwd      {cwd_s}\n  attach   trk attach {id}\n  steer    trk send {id} \"…\"  or  trk note {id} --to-agent \"…\"",
    );
    if let Some(placement) = &placement {
        text.push_str(&format!("\n  ui       {}", placement.description()));
    }
    Ok(Out::Text(
        text,
        json!({"issue": id, "agent": agent_name, "harness": hname, "actor": actor, "terminal": terminal,
               "worktree": wt_str, "session_dir": spec.session_dir, "launcher": l.name, "launcher_output": output, "briefing_sent": sent,
               "placement": placement, "type": routing.issue_type, "routing": routing,
               "warnings": warnings}),
    ))
}

pub fn pid_alive(pid: i64) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Liveness of a run's MCP server: Some(true) alive, Some(false) gone, None unknown.
pub fn mcp_alive(meta: &Value) -> Option<bool> {
    if meta["mcp_exited_at"].is_string() {
        return Some(false);
    }
    let pid = meta["mcp_pid"].as_i64()?;
    let host = meta["mcp_host"].as_str();
    if host.is_some() && host.map(String::from) != hostname() {
        return None;
    }
    Some(pid_alive(pid))
}

/// Session liveness comes only from a known local terminal, never its MCP server.
pub fn session_alive(
    launcher: &Launcher,
    host: Option<&str>,
    terminal: Option<&str>,
) -> Option<bool> {
    let host = host.filter(|host| !host.trim().is_empty())?;
    if hostname().as_deref() != Some(host) {
        return None;
    }
    let terminal = terminal.filter(|name| !name.trim().is_empty())?;
    launcher.exists(terminal)
}

pub fn runs(env: &Env) -> Result<Out> {
    let store = env.store()?;
    let mut runs = store.read(None, |c| c.runs())?;
    for r in &mut runs {
        r["mcp_alive"] = json!(mcp_alive(&r["meta"]));
        r["session_alive"] = json!(session_alive(
            &launcher::for_claim(env, r["launcher"].as_str())?,
            r["host"].as_str(),
            r["terminal"].as_str(),
        ));
    }
    let mut t = comfy_table::Table::new();
    t.load_style(comfy_table::presets::NOTHING);
    t.set_header([
        "ISSUE", "STATE", "AGENT", "HARNESS", "TERMINAL", "LEASE", "SESSION", "MCP", "TITLE",
    ]);
    for r in &runs {
        t.add_row([
            r["issue"].as_str().unwrap_or("").to_string(),
            r["state"].as_str().unwrap_or("").to_string(),
            r["agent"].as_str().unwrap_or("").to_string(),
            r["harness"].as_str().unwrap_or("").to_string(),
            r["terminal"].as_str().unwrap_or("").to_string(),
            r["lease_until"].as_str().unwrap_or("").to_string(),
            match r["session_alive"].as_bool() {
                Some(true) => "live".into(),
                Some(false) => "ended".into(),
                None => "?".into(),
            },
            match r["mcp_alive"].as_bool() {
                Some(true) => "up".into(),
                Some(false) if r["session_alive"] == true => "dormant".into(),
                Some(false) => "stopped".into(),
                None => "?".into(),
            },
            r["title"].as_str().unwrap_or("").to_string(),
        ]);
    }
    let text = if runs.is_empty() {
        "no active or waiting agent sessions".to_string()
    } else {
        t.to_string()
    };
    Ok(Out::Text(text, Value::Array(runs)))
}

fn terminal_of(env: &Env, id: &str) -> Result<(String, Claim)> {
    env.store()?.read(None, |c| {
        let id = c.resolve_id(id)?;
        let claim = c
            .session_claim(&id)?
            .filter(|claim| claim.terminal.is_some())
            .ok_or_else(|| {
                Error::new(
                    Code::NotFound,
                    format!("{id} has no running session terminal"),
                )
            })?;
        Ok((id, claim))
    })
}

pub fn attach(env: &Env, id: &str) -> Result<Out> {
    let (id, claim) = terminal_of(env, id)?;
    let name = claim.terminal.as_deref().expect("filtered terminal");
    let l = launcher::for_claim(env, claim.launcher.as_deref())?;
    if !l.focus(name)? {
        if let Some((false, _)) = l.operation(l.attach.as_deref(), name, None, true)? {
            return Err(
                Error::new(Code::Internal, format!("attach to {name} failed")).with_issue(&id),
            );
        }
    }
    Ok(Out::Silent(json!({"issue": id, "terminal": name})))
}

pub fn send(env: &Env, id: &str, text: &str) -> Result<Out> {
    let (id, claim) = terminal_of(env, id)?;
    let l = launcher::for_claim(env, claim.launcher.as_deref())?;
    let name = claim.terminal.expect("filtered terminal");
    let Some(tmpl) = l.send.as_deref().filter(|tmpl| !tmpl.is_empty()) else {
        return Ok(Out::Silent(
            json!({"issue": id, "terminal": name, "supported": false}),
        ));
    };
    let mut vars = BTreeMap::new();
    vars.insert("name", name.clone());
    vars.insert("text", text.to_string());
    let (ok, out) = send_with_retry(tmpl, &vars, l.send_retries, l.send_retry_delay)?;
    if !ok {
        return Err(
            Error::new(Code::Internal, format!("send to {name} failed: {out}")).with_issue(&id),
        );
    }
    Ok(Out::Text(
        format!("sent to {name}"),
        json!({"issue": id, "terminal": name, "output": out}),
    ))
}

pub fn resume(env: &Env, id: &str, message: &str) -> Result<Out> {
    let actor = env.actor()?.to_string();
    if ActorId::parse(&actor)?.is_agent() {
        return Err(Error::new(Code::NotPermitted, "only a human may authorize trk resume")
            .with_issue(id)
            .with_hint("ask the human to run `trk resume <ID> -m \"continuation instructions\"`; chat or restored access alone does not restore a claim"));
    }
    let message = message.trim();
    if message.is_empty() {
        return Err(trk_core::error::bad_args(
            "resume needs a nonempty continuation message",
        ));
    }
    crate::check::with_queue_lock(env, || {
        let mut store = env.store()?;
        let (id, revision, previous) = store.read(None, |c| {
            let id = c.resolve_id(id)?;
            let issue = c.issue(&id)?;
            if issue.state != State::Waiting || c.active_claim(&id)?.is_some() {
                return Err(Error::new(
                    Code::InvalidState,
                    format!(
                        "{id} is {}; only an unclaimed waiting issue can resume",
                        issue.state
                    ),
                )
                .with_issue(&id));
            }
            let previous = c.session_claim(&id)?;
            Ok((id.clone(), c.issue_revision(&id)?, previous))
        })?;
        let unavailable = || {
            Error::new(Code::InvalidState, format!("{id}: original waiting session is absent, not live locally, or cannot receive instructions"))
                .with_issue(&id)
                .with_hint(format!("if the original session is gone, a human must run `trk reopen {id} -m \"start a replacement\"` then `trk run {id}`; otherwise restore local session/terminal/send access and retry resume"))
        };
        let previous = previous.ok_or_else(unavailable)?;
        if !ActorId::parse(&previous.actor)?.is_agent() {
            return Err(unavailable());
        }
        let session = live_waiting_session(
            &previous,
            launcher::for_claim(env, previous.launcher.as_deref())?,
        )
        .ok_or_else(unavailable)?;
        let claim = store.write(&actor, false, |c| {
            c.resume(&id, previous.id, revision, message)
        })?;
        let instructions = format!(
            "Human-authorized continuation for {id}. Your original session's claim and MCP tools have been restored with a fresh lease. \
             Continue using the issue's notes and these human instructions. End this new cycle with close, wait, or hand_back as appropriate. \
             This manual resume does not consume or reset the automatic failed-check wake budget.\n\n{message}"
        );
        if let Err(error) = session.send(&instructions) {
            let reopened = store.write(&actor, false, |c| {
                c.resume_failed(&id, claim.id, &error.to_string())
            })?;
            return Err(Error::new(Code::Internal, format!("resume delivery failed: {error}"))
                .with_issue(&id)
                .with_detail("claim", claim.id)
                .with_detail("reopened", reopened)
                .with_hint(if reopened {
                    format!("the restored claim was released; a human may run `trk run {id}`")
                } else {
                    format!("the restored claim changed; inspect `trk show {id}` before further action")
                }));
        }
        store.write(&actor, false, |c| c.resume_delivered(&id, claim.id))?;
        Ok(Out::Text(
            format!("resumed {id} in {}", session.terminal),
            json!({"issue": id, "actor": claim.actor, "terminal": claim.terminal,
                   "claim": claim.id, "resumed_from": previous.id, "message": message,
                   "delivered": true}),
        ))
    })
}
/// A live original waiting session, selected before restoring its claim.
pub struct WaitingSession {
    pub claim: i64,
    terminal: String,
    send: Vec<String>,
    send_retries: Option<u64>,
    send_retry_delay: Option<u64>,
}

impl WaitingSession {
    pub fn send(&self, text: &str) -> Result<()> {
        let mut vars = BTreeMap::new();
        vars.insert("name", self.terminal.clone());
        vars.insert("text", text.to_string());
        let (ok, out) =
            send_with_retry(&self.send, &vars, self.send_retries, self.send_retry_delay)?;
        if !ok {
            return Err(Error::new(
                Code::Internal,
                format!("send to {} failed: {out}", self.terminal),
            ));
        }
        Ok(())
    }
}

fn waiting_claim(env: &Env, issue: &str) -> Result<Option<Claim>> {
    env.store()?.read(None, |c| {
        Ok(c.latest_claim(issue)?
            .filter(|claim| claim.release_reason.as_deref() == Some("waiting")))
    })
}

fn live_waiting_session(claim: &Claim, launcher: Launcher) -> Option<WaitingSession> {
    let name = claim.terminal.as_ref()?;
    let send = launcher.send.as_ref()?;
    if send.is_empty() || session_alive(&launcher, claim.host.as_deref(), Some(name)) != Some(true)
    {
        return None;
    }
    Some(WaitingSession {
        claim: claim.id,
        terminal: name.clone(),
        send: launcher.send?,
        send_retries: launcher.send_retries,
        send_retry_delay: launcher.send_retry_delay,
    })
}

pub fn waiting_session(env: &Env, issue: &str) -> Result<Option<WaitingSession>> {
    let Some(claim) = waiting_claim(env, issue)? else {
        return Ok(None);
    };
    Ok(live_waiting_session(
        &claim,
        launcher::for_claim(env, claim.launcher.as_deref())?,
    ))
}

/// Best-effort UI update and notification to the session that left an issue waiting.
///
/// The waiting claim is historical by design: resolving the issue releases it,
/// so looking up the active claim could steer a replacement session instead.
pub fn notify_waiting_session(env: &Env, issue: &str, text: &str, closed: bool) -> Result<()> {
    let Some(claim) = waiting_claim(env, issue)? else {
        return Ok(());
    };
    let Some(name) = &claim.terminal else {
        return Ok(());
    };
    let launcher = launcher::for_claim(env, claim.launcher.as_deref())?;
    // Renaming is independent of agent liveness and message delivery.
    if closed {
        let title = env.store()?.read(None, |c| Ok(c.issue(issue)?.title))?;
        let _ = launcher.rename(name, &format!("[closed] {title}"));
    }
    if let Some(session) = live_waiting_session(&claim, launcher) {
        session.send(text)?;
    }
    Ok(())
}

/// Release claims whose terminal is known to be gone, recording it as a note.
pub fn reap(env: &Env) -> Result<Out> {
    let actor = env.actor()?.to_string();
    let mut store = env.store()?;
    let runs = store.read(None, |c| c.runs())?;
    let mut reaped = vec![];
    for r in runs {
        if r["state"] != "claimed"
            || session_alive(
                &launcher::for_claim(env, r["launcher"].as_str())?,
                r["host"].as_str(),
                r["terminal"].as_str(),
            ) != Some(false)
        {
            continue;
        }
        let id = r["issue"].as_str().unwrap_or_default().to_string();
        let why = format!(
            "Session of {} ended: its terminal {} is gone. Claim released by trk reap.",
            r["actor"].as_str().unwrap_or(""),
            r["terminal"].as_str().unwrap_or(""),
        );
        store.write(&actor, false, |c| c.reap(&id, &why))?;
        reaped.push(json!({"issue": id, "actor": r["actor"], "note": why}));
    }
    let text = if reaped.is_empty() {
        "nothing to reap".to_string()
    } else {
        reaped
            .iter()
            .map(|r| {
                format!(
                    "released {} ({})",
                    r["issue"].as_str().unwrap_or(""),
                    r["actor"].as_str().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(Out::Text(text, Value::Array(reaped)))
}
