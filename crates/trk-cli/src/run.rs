//! `trk run` and friends: launcher templates and optional Herdr spaces.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use serde_json::{Value, json};
use trk_agent::harness::{self, LaunchInput, subst};
use trk_core::error::{Code, Error, Result};
use trk_core::fields::Assign;
use trk_core::model::normalize_remote;
use trk_core::ops::ClaimOpts;
use trk_core::view::With;

use crate::env::{Env, git, hostname, state_dir};
use crate::render::Out;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Launcher {
    pub start: Option<Vec<String>>,
    pub attach: Option<Vec<String>>,
    pub send: Option<Vec<String>>,
    #[serde(default)]
    pub send_delay: Option<u64>,
    /// Launch directly in a new Herdr space when its UI is available.
    #[serde(default)]
    pub herdr: bool,
}

pub fn launcher(env: &Env) -> Result<Launcher> {
    match &env.user.launcher {
        None => Ok(Launcher::default()),
        Some(v) => v
            .clone()
            .try_into()
            .map_err(|e| Error::new(Code::SchemaInvalid, format!("config.toml [launcher]: {e}"))),
    }
}

fn run_template(
    tmpl: &[String],
    vars: &BTreeMap<&str, String>,
    inherit: bool,
) -> Result<(bool, String)> {
    let argv: Vec<String> = tmpl.iter().map(|a| subst(a, vars)).collect();
    let (prog, args) = argv
        .split_first()
        .ok_or_else(|| Error::new(Code::SchemaInvalid, "empty launcher template"))?;
    if inherit {
        let st = Command::new(prog)
            .args(args)
            .status()
            .map_err(|e| Error::new(Code::Internal, format!("cannot run {prog}: {e}")))?;
        return Ok((st.success(), String::new()));
    }
    let out = Command::new(prog)
        .args(args)
        .output()
        .map_err(|e| Error::new(Code::Internal, format!("cannot run {prog}: {e}")))?;
    let mut text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !err.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&err);
    }
    Ok((out.status.success(), text))
}

/// A session that is still starting may refuse input; retry for ~30s.
fn send_with_retry(tmpl: &[String], vars: &BTreeMap<&str, String>) -> Result<(bool, String)> {
    let mut last = (false, String::new());
    for attempt in 0..10 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
        last = run_template(tmpl, vars, false)?;
        if last.0 {
            break;
        }
    }
    Ok(last)
}

pub struct RunArgs {
    pub id: String,
    pub agent: Option<String>,
    pub harness: Option<String>,
    pub fg: bool,
    pub print: bool,
    pub dry_run: bool,
    pub assigns: Vec<Assign>,
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

/// The git checkout an issue's worktree is made from, if any.
fn checkout_for(env: &Env, store: &trk_core::Store, issue: &Value) -> Result<Option<PathBuf>> {
    let project = issue["project"].as_str().unwrap_or_default().to_string();
    if let Some(r) = issue["fields"]["repo"].as_str() {
        let repo = store.read(None, |c| c.repo_by_ref(r))?;
        if let Some(repo) = &repo {
            if let Some(lp) = &repo.local_path
                && Path::new(lp).join(".git").exists()
            {
                return Ok(Some(PathBuf::from(lp)));
            }
            if let Some(g) = &env.git {
                let same_remote = g.remote.as_deref().map(normalize_remote) == repo.remote_url;
                let same_path = repo.local_path.as_deref() == Some(g.toplevel.as_str());
                if same_remote || same_path {
                    return Ok(Some(PathBuf::from(&g.toplevel)));
                }
            }
        }
        return Err(Error::new(Code::UnknownRepo, format!("no local checkout of {r} found"))
            .with_hint("run `trk run` inside a checkout of the repo, or register its path with `trk repo add <path>`"));
    }
    if let Some(g) = &env.git
        && let Some(repo) = env.current_repo(store)?
        && repo.project == project
    {
        return Ok(Some(PathBuf::from(&g.toplevel)));
    }
    Ok(None)
}

fn ensure_worktree(checkout: &Path, path: &Path, branch: &str) -> Result<()> {
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
        vec!["worktree", "add", &p, "-b", branch]
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
    let (issue, ty) = store.read(Some(&human), |c| {
        let id = c.resolve_id(&a.id)?;
        let v = c.show(
            &id,
            With {
                deps: true,
                ..Default::default()
            },
        )?;
        let ty = c.schema.ty(v["type"].as_str().unwrap_or_default())?.clone();
        Ok((v, ty))
    })?;
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
            return Err(Error::new(Code::GateUnmet, format!("{id} is blocked"))
                .with_issue(&id)
                .with_detail("blocked_by", Value::Array(b.clone())));
        }
        return Err(Error::new(
            Code::InvalidTransition,
            format!(
                "{id} is '{}', not ready",
                issue["status"].as_str().unwrap_or("")
            ),
        )
        .with_issue(&id));
    }
    if !ty.claimable() {
        return Err(Error::new(
            Code::InvalidTransition,
            format!(
                "type '{}' cannot be claimed, so no agent can run on it",
                ty.name
            ),
        ));
    }
    let agent_name = a
        .agent
        .clone()
        .or_else(|| issue["fields"]["agent"].as_str().map(String::from))
        .or_else(|| ty.agent.clone())
        .ok_or_else(|| {
            Error::new(Code::NotFound, format!("no agent for {id}")).with_hint(format!(
                "pass --agent, or set routing.agent in types/{}.toml",
                ty.name
            ))
        })?;
    let agent = trk_agent::load_agent(&env.config, &agent_name)?;
    let profiles = harness::profiles(&env.user)?;
    let hname = harness::choose(&agent, &profiles, a.harness.as_deref())?;
    let profile = profiles[&hname].clone();

    // 2. Worktree.
    let checkout = checkout_for(env, &store, &issue)?;
    let branch = issue["fields"]["branch"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| id.clone());
    let worktree: Option<PathBuf> = checkout.as_ref().map(|_| env.worktrees_dir().join(&id));

    // 3. Actor.
    let uuid = uuid::Uuid::new_v4().to_string();
    let actor = format!("agent:{hname}:{uuid}");
    let mut terminal = terminal_name(&id);
    let session_dir = state_dir().join("sessions").join(&uuid);
    let cwd = worktree.clone().unwrap_or_else(|| session_dir.clone());

    let mut warnings: Vec<String> = vec![];
    if !a.dry_run {
        if let (Some(co), Some(wt)) = (&checkout, &worktree) {
            ensure_worktree(co, wt, &branch)?;
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
        steal: false,
        branch: worktree.as_ref().map(|_| branch.clone()),
        on_behalf_of: Some(human.clone()),
        assigns: a.assigns.clone(),
    };
    // In a dry run the claim is rolled back, but the issue is rendered as the
    // agent would see it after claiming.
    let (issue_now, tools) = store.write(&actor, a.dry_run, |c| {
        c.claim(&id, claim_opts)?;
        let v = c.show(
            &id,
            With {
                deps: true,
                ..Default::default()
            },
        )?;
        Ok((v, trk_agent::agent_tools(&c.schema, &ty, &agent)))
    })?;

    // 5. Render the launch spec.
    let input = trk_agent::prompt::PromptInput {
        issue: &issue_now,
        agent: &agent,
        tools: &tools,
        worktree: wt_str.as_deref(),
        actor: &actor,
        harness: Some(&hname),
    };
    let instructions = trk_agent::prompt::instructions(&env.config, &input)?;
    let fold = if profile.server_instructions {
        None
    } else {
        Some(instructions.as_str())
    };
    let briefing = trk_agent::prompt::briefing(&env.config, &input, fold)?;
    let mut trk_env = BTreeMap::new();
    trk_env.insert(
        "TRK_DB".to_string(),
        std::fs::canonicalize(&env.db)
            .unwrap_or(env.db.clone())
            .to_string_lossy()
            .to_string(),
    );
    trk_env.insert(
        "TRK_CONFIG".to_string(),
        std::fs::canonicalize(&env.config.path)
            .unwrap_or(env.config.path.clone())
            .to_string_lossy()
            .to_string(),
    );
    let cwd_s = cwd.to_string_lossy().to_string();
    let spec = harness::build(LaunchInput {
        issue: &id,
        agent: &agent,
        harness: &hname,
        profile: &profile,
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
    let spec_json = serde_json::to_value(&spec)?;
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
        if let Some(wt) = &worktree
            && let Ok(rel) = p.strip_prefix(wt)
        {
            crate::env::git_exclude(wt, &format!("/{}", rel.to_string_lossy()));
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
    let l = launcher(env)?;
    let mut vars = BTreeMap::new();
    vars.insert("name", terminal.clone());
    vars.insert("cwd", cwd_s.clone());
    vars.insert("shell_command", spec.shell_command.clone());
    let ui = l.herdr.then(crate::herdr::available);
    let (output, placement) = if matches!(ui, Some(Ok(()))) {
        let title = issue["title"].as_str().unwrap_or(&id);
        let (address, placement) = crate::herdr::start(title, &cwd_s, &spec.shell_command)
            .map_err(|reason| {
                Error::new(Code::Internal, format!("Herdr launch unconfirmed: {reason}"))
                    .with_issue(&id)
                    .with_hint("claim retained to avoid a duplicate launch; inspect `herdr-app ls` and `hc ls` before unclaiming")
            })?;
        store.write(&actor, false, |c| c.bind_terminal(&id, &address))?;
        terminal = address;
        vars.insert("name", terminal.clone());
        (String::new(), Some(placement))
    } else {
        let Some(start) = &l.start else {
            undo(&mut store, "launch failed: no [launcher] start template");
            return Err(Error::new(
                Code::SchemaInvalid,
                "no [launcher] start template in config.toml",
            )
            .with_hint("use --fg to run in this terminal, or configure [launcher]"));
        };
        let (ok, output) = run_template(start, &vars, false)?;
        if !ok {
            undo(&mut store, &format!("launch failed: {output}"));
            return Err(
                Error::new(Code::Internal, format!("launcher start failed: {output}")).with_issue(&id),
            );
        }
        let placement = ui.map(|result| crate::herdr::Placement::Failed {
            reason: result.unwrap_err(),
        });
        (output, placement)
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
        let (ok, out) = send_with_retry(send, &vars)?;
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
               "worktree": wt_str, "session_dir": spec.session_dir, "launcher_output": output, "briefing_sent": sent,
               "placement": placement,
               "warnings": warnings}),
    ))
}

fn pid_alive(pid: i64) -> bool {
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

pub fn runs(env: &Env) -> Result<Out> {
    let store = env.store()?;
    let mut runs = store.read(None, |c| c.runs())?;
    for r in &mut runs {
        r["mcp_alive"] = json!(mcp_alive(&r["meta"]));
    }
    let mut t = comfy_table::Table::new();
    t.load_style(comfy_table::presets::NOTHING);
    t.set_header([
        "ISSUE", "AGENT", "HARNESS", "TERMINAL", "STATUS", "LEASE", "MCP", "TITLE",
    ]);
    for r in &runs {
        t.add_row([
            r["issue"].as_str().unwrap_or("").to_string(),
            r["agent"].as_str().unwrap_or("").to_string(),
            r["harness"].as_str().unwrap_or("").to_string(),
            r["terminal"].as_str().unwrap_or("").to_string(),
            r["status"].as_str().unwrap_or("").to_string(),
            r["lease_until"].as_str().unwrap_or("").to_string(),
            match r["mcp_alive"].as_bool() {
                Some(true) => "up".into(),
                Some(false) => "gone".into(),
                None => "?".into(),
            },
            r["title"].as_str().unwrap_or("").to_string(),
        ]);
    }
    let text = if runs.is_empty() {
        "no active agent runs".to_string()
    } else {
        t.to_string()
    };
    Ok(Out::Text(text, Value::Array(runs)))
}

fn terminal_of(env: &Env, id: &str) -> Result<(String, String)> {
    let store = env.store()?;
    store.read(None, |c| {
        let id = c.resolve_id(id)?;
        let claim = c.active_claim(&id)?;
        let t = claim.and_then(|c| c.terminal).ok_or_else(|| {
            Error::new(
                Code::NotFound,
                format!("{id} has no running session terminal"),
            )
        })?;
        Ok((id, t))
    })
}

pub fn attach(env: &Env, id: &str) -> Result<Out> {
    let l = launcher(env)?;
    let tmpl = l
        .attach
        .ok_or_else(|| Error::new(Code::SchemaInvalid, "no [launcher] attach template"))?;
    let (id, name) = terminal_of(env, id)?;
    let mut vars = BTreeMap::new();
    vars.insert("name", name.clone());
    let (ok, _) = run_template(&tmpl, &vars, true)?;
    if !ok {
        return Err(Error::new(Code::Internal, format!("attach to {name} failed")).with_issue(&id));
    }
    Ok(Out::Silent(json!({"issue": id, "terminal": name})))
}

pub fn send(env: &Env, id: &str, text: &str) -> Result<Out> {
    let l = launcher(env)?;
    let tmpl = l
        .send
        .ok_or_else(|| Error::new(Code::SchemaInvalid, "no [launcher] send template"))?;
    let (id, name) = terminal_of(env, id)?;
    let mut vars = BTreeMap::new();
    vars.insert("name", name.clone());
    vars.insert("text", text.to_string());
    let (ok, out) = send_with_retry(&tmpl, &vars)?;
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

/// Release claims whose MCP server has gone, recording the exit as a note.
pub fn reap(env: &Env) -> Result<Out> {
    let actor = env.actor()?.to_string();
    let mut store = env.store()?;
    let runs = store.read(None, |c| c.runs())?;
    let mut reaped = vec![];
    for r in runs {
        if mcp_alive(&r["meta"]) != Some(false) {
            continue;
        }
        let id = r["issue"].as_str().unwrap_or_default().to_string();
        let why = match r["meta"]["mcp_exited_at"].as_str() {
            Some(t) => format!(
                "Session of {} ended: its MCP server exited at {t}. Claim released by trk reap.",
                r["actor"].as_str().unwrap_or("")
            ),
            None => format!(
                "Session of {} ended: its MCP server (pid {}) is gone. Claim released by trk reap.",
                r["actor"].as_str().unwrap_or(""),
                r["meta"]["mcp_pid"]
            ),
        };
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
