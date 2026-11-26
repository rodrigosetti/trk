//! `trk run` and friends: launcher profiles and persistent sessions.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use trk_agent::harness::{self, LaunchInput};
use trk_core::error::{Code, Error, Result};
use trk_core::model::{ActorId, Claim, State};
use trk_core::ops::ClaimOpts;
use trk_core::system::{Liveness, hostname, process_liveness};
use trk_core::view::{ClaimView, IssueView, McpStatus, RunView, With};

use crate::env::{Env, git};
use crate::render::Out;

use crate::launcher::{self, Launcher, run_template};

/// Terminal deliveries are machine-originated even when a human invokes the CLI.
fn delivery(source: &str, actor: &str, text: &str) -> String {
    format!("[trk-delivery source={source} actor={actor:?}]\n{text}\n[/trk-delivery]")
}

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

/// Keep terminal ownership unchanged, but allow trk to clean up after interruption.
fn foreground_status(shell_command: &str) -> std::io::Result<ExitStatus> {
    struct Signals(Vec<signal_hook::SigId>);
    impl Drop for Signals {
        fn drop(&mut self) {
            for handler in self.0.drain(..) {
                signal_hook::low_level::unregister(handler);
            }
        }
    }

    let signal = Arc::new(AtomicUsize::new(0));
    let mut handlers = Signals(Vec::with_capacity(3));
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        handlers.0.push(signal_hook::flag::register_usize(
            sig,
            Arc::clone(&signal),
            sig as usize,
        )?);
    }
    let mut child = Command::new("sh").arg("-c").arg(shell_command).spawn()?;
    let mut interrupted = None;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        let sig = signal.load(Ordering::Relaxed);
        if sig != 0 && interrupted.is_none() {
            // The launch command execs the harness. It may already have received
            // the terminal's signal; forwarding also covers signals sent only to trk.
            unsafe { libc::kill(child.id() as libc::pid_t, sig as libc::c_int) };
            interrupted = Some(Instant::now());
        }
        if interrupted.is_some_and(|at| at.elapsed() >= Duration::from_secs(2)) {
            child.kill()?;
            return child.wait();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn run(env: &Env, a: RunArgs) -> Result<Out> {
    let human = env.actor()?.to_string();
    let mut store = env.store()?;
    let IssueView {
        id,
        title,
        state,
        claim,
        derived,
        issue_type,
        ..
    } = if let Some(id) = a.id.as_deref() {
        store.read(Some(&human), |c| {
            let id = c.resolve_id(id)?;
            c.issue_view(
                &c.issue(&id)?,
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

    // 1. Ready, agent and harness.
    if !derived.ready {
        if let Some(c) = &claim {
            return Err(Error::new(
                Code::ClaimedByOther,
                format!("{id} is claimed by {}", c.actor),
            )
            .with_issue(&id)
            .with_hint(format!("see `trk runs`; `trk attach {id}` to take over")));
        }
        let b = &derived.blocked_by;
        if !b.is_empty() {
            return Err(Error::new(Code::Blocked, format!("{id} is blocked"))
                .with_issue(&id)
                .with_detail("blocked_by", json!(b)));
        }
        let mut e =
            Error::new(Code::InvalidState, format!("{id} is {state}, not ready")).with_issue(&id);
        if state != State::Open {
            e = e.with_hint(format!("trk reopen {id}"));
        }
        return Err(e);
    }
    let routing_config = trk_agent::routing::RoutingConfig::new(&env.config);
    let routing = routing_config.resolve(&issue_type, a.agent.as_deref())?;
    let resolved = routing_config.load(&routing)?;
    let agent = resolved.agent;
    let agent_name = agent.name.clone();
    let profiles = harness::profiles(&env.user)?;
    let hname = harness::choose(&agent, &profiles, a.harness.as_deref())?;
    let profile = profiles[&hname].clone();
    profile.builtin_args(&agent.harness.builtin_tools, &hname)?;

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
    let project = env.root.file_name().unwrap_or_default().to_string_lossy();
    let default_naming = Launcher::default();
    let naming = selected.as_ref().unwrap_or(&default_naming);
    let mut terminal = naming.terminal_name(&[
        ("id", &id),
        ("prefix", &env.user.issues.prefix),
        ("project", &project),
        ("agent", &agent_name),
        ("type", &routing.issue_type),
    ]);
    let session_dir = env.sessions_dir().join(&uuid);
    let cwd = if let (Some(wt), Some(project)) = (&worktree, &env.project_git) {
        let root = std::fs::canonicalize(&env.root)?;
        let relative = root.strip_prefix(&project.toplevel).map_err(|e| {
            Error::new(
                Code::Internal,
                format!("project is outside its checkout: {e}"),
            )
        })?;
        if relative.as_os_str().is_empty() {
            wt.clone()
        } else {
            wt.join(relative)
        }
    } else if routing.worktree {
        session_dir.clone()
    } else {
        env.root.clone()
    };

    let mut warnings: Vec<String> = vec![];
    if !a.dry_run {
        if let (Some(co), Some(wt)) = (&checkout, &worktree) {
            // The branch is always the issue id: a later session finds the same one.
            ensure_worktree(co, wt, &id, &base)?;
            std::fs::create_dir_all(&cwd)?;
            // Agents reach trk through MCP only; a committed skill would reach them anyway.
            for rel in crate::skill::paths(&env.user.skill) {
                let full = env.root.join(rel);
                if let Ok(relative) = full.strip_prefix(&env.root)
                    && git(&cwd, &["ls-files", "--", &relative.to_string_lossy()])
                        .is_some_and(|s| !s.is_empty())
                {
                    let w = format!(
                        "{rel} is tracked in this repo, so the session will see the trk CLI skill"
                    );
                    eprintln!("warning: {w}");
                    warnings.push(w);
                }
            }
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&session_dir)?;
    }

    // 4. Claim.
    let wt_str = worktree.as_ref().map(|w| w.to_string_lossy().to_string());
    let claim_opts = ClaimOpts {
        lease: None,
        worktree: wt_str.clone(),
        host: hostname().map(str::to_owned),
        agent: Some(agent_name.clone()),
        terminal: selected.as_ref().map(|_| terminal.clone()),
        launcher: selected.as_ref().and_then(|launcher| launcher.name.clone()),
        steal: false,
        on_behalf_of: Some(human.clone()),
    };
    // In a dry run the claim is rolled back, but the issue is rendered as the
    // agent would see it after claiming.
    let tools = trk_agent::agent_tools(&agent, &env.user);
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

    let undo = |store: &mut trk_core::Store, why: &str| {
        if !a.dry_run {
            let _ = store.write(&actor, false, |c| c.unclaim(&id, false, Some(why)));
        }
    };

    // 5. Render the launch spec and write the files the harness reads.
    let cwd_s = cwd.to_string_lossy().to_string();
    let (spec, spec_json) = (|| -> Result<_> {
        let epics = store.read(Some(&actor), |c| c.epics(false))?;
        let input = trk_agent::prompt::PromptInput {
            issue: &issue_now,
            epics: &epics,
            project_root: &env.root,
            config: &env.user,
            issue_type: &routing.issue_type,
            type_description: &routing.description,
            type_guidance: routing.guidance.as_deref(),
            agent: &agent,
            tools: &tools,
            worktree: wt_str.as_deref(),
            actor: &actor,
            harness: Some(&hname),
            base: &base,
            verify: &env.user.check.verify,
        };
        let prompts = trk_agent::prompt::render_prompts(
            &resolved.config,
            &input,
            !profile.server_instructions,
        )?;
        let mut trk_env = BTreeMap::new();
        trk_env.insert("TRK_DIR".to_string(), env.dir_env());
        if let Some(user_dir) = &env.config.user_path {
            trk_env.insert(
                "TRK_USER_DIR".to_string(),
                user_dir.to_string_lossy().into_owned(),
            );
        }
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
            briefing: prompts.briefing,
            instructions: prompts.instructions,
            tools: tools.iter().map(|t| t.name.clone()).collect(),
        })?;
        let mut spec_json = serde_json::to_value(&spec)?;
        spec_json["type"] = json!(routing.issue_type);
        spec_json["routing"] = json!(routing);
        spec_json["templates"] = serde_json::to_value(prompts.sources)?;
        if let Some(guidance) = prompts.guidance {
            spec_json["guidance"] = json!(guidance);
        }
        if !a.dry_run {
            for (path, content) in &spec.files {
                let p = PathBuf::from(path);
                if let Some(d) = p.parent() {
                    std::fs::DirBuilder::new()
                        .recursive(true)
                        .mode(0o700)
                        .create(d)?;
                }
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .mode(0o600)
                    .open(&p)?;
                // Restrict existing files before replacing their contents.
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                file.set_len(0)?;
                file.write_all(content.as_bytes())?;
                if checkout.is_some()
                    && let Ok(rel) = p.strip_prefix(&cwd)
                {
                    crate::env::git_exclude(&cwd, &format!("/{}", rel.to_string_lossy()));
                }
            }
        }
        Ok((spec, spec_json))
    })()
    .inspect_err(|error| undo(&mut store, &format!("launch failed: {error}")))?;
    if a.dry_run || a.print {
        return Ok(Out::Data(spec_json));
    }

    // 6. Start.
    if a.fg {
        let st = foreground_status(&spec.shell_command)
            .inspect_err(|error| undo(&mut store, &format!("launch failed: {error}")))?;
        let released = store.write(&actor, false, |c| {
            if c.active_claim(&id)?
                .is_some_and(|claim| claim.actor == actor)
            {
                c.unclaim(
                    &id,
                    false,
                    Some(&format!("foreground session exited ({st})")),
                )?;
                Ok(true)
            } else {
                Ok(false)
            }
        })?;
        let message = if released {
            "Its claim was released."
        } else {
            "No active claim belonging to this session remains."
        };
        return Ok(Out::Text(
            format!("{hname} exited ({st}). {message}"),
            json!({"issue": id, "actor": actor, "exit_code": st.code()}),
        ));
    }
    let l = selected.expect("background launch selects a profile");
    let mut vars = BTreeMap::new();
    vars.insert("name", terminal.clone());
    vars.insert("cwd", cwd_s.clone());
    vars.insert("shell_command", spec.shell_command.clone());
    let title = title.as_str();
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
        let start = l.start.as_deref().expect("validated launcher start");
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
        let address = l.start_address(&out.stdout).map_err(|error| {
            error.with_issue(&id).with_hint(
                "claim retained to avoid a duplicate launch; inspect the launcher before unclaiming",
            )
        })?;
        if let Some(address) = address {
            store.write(&actor, false, |c| c.bind_terminal(&id, &address))?;
            terminal = address;
            vars.insert("name", terminal.clone());
        }
        (out.text, None)
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
        vars.insert("text", delivery("briefing", env.actor()?, &spec.briefing));
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

/// Liveness of a run's MCP server: Some(true) alive, Some(false) gone, None unknown.
pub fn mcp_alive(meta: &Value) -> Option<bool> {
    let meta = McpStatus::from(meta);
    if meta.exited_at.is_some() {
        return Some(false);
    }
    let pid = meta.pid?;
    let host = meta.host;
    if host.is_some() && host != hostname() {
        return None;
    }
    // Preserve false for invalid IDs or other unknown probe failures.
    Some(process_liveness(pid) == Liveness::Alive)
}

/// Session liveness comes only from a known local terminal, never its MCP server.
pub fn session_alive(
    launcher: &Launcher,
    host: Option<&str>,
    terminal: Option<&str>,
) -> Option<bool> {
    let host = host.filter(|host| !host.trim().is_empty())?;
    if hostname() != Some(host) {
        return None;
    }
    let terminal = terminal.filter(|name| !name.trim().is_empty())?;
    launcher.exists(terminal)
}

fn claim_session_alive(env: &Env, claim: &ClaimView) -> Result<Option<bool>> {
    let Some(terminal) = claim
        .terminal
        .as_deref()
        .filter(|name| !name.trim().is_empty())
    else {
        return Ok(None);
    };
    let Some(launcher) = launcher::for_claim(env, claim.launcher.as_deref())? else {
        return Ok(None);
    };
    Ok(session_alive(
        &launcher,
        claim.host.as_deref(),
        Some(terminal),
    ))
}

#[derive(serde::Serialize)]
struct RunStatus {
    #[serde(flatten)]
    run: RunView,
    mcp_alive: Option<bool>,
    session_alive: Option<bool>,
}

pub fn runs(env: &Env) -> Result<Out> {
    let store = env.store()?;
    let runs = store
        .read(None, |c| c.runs())?
        .into_iter()
        .map(|run| {
            Ok(RunStatus {
                mcp_alive: mcp_alive(&run.meta),
                session_alive: claim_session_alive(env, &run.claim)?,
                run,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut t = comfy_table::Table::new();
    t.load_style(comfy_table::presets::NOTHING);
    t.set_header([
        "ISSUE", "STATE", "AGENT", "HARNESS", "TERMINAL", "LEASE", "SESSION", "MCP", "TITLE",
    ]);
    for r in &runs {
        t.add_row([
            r.run.issue.clone(),
            r.run.state.to_string(),
            r.run.claim.agent.clone().unwrap_or_default(),
            r.run.harness.clone().unwrap_or_default(),
            r.run.claim.terminal.clone().unwrap_or_default(),
            r.run.claim.lease_until.clone().unwrap_or_default(),
            match r.session_alive {
                Some(true) => "live".into(),
                Some(false) => "ended".into(),
                None => "?".into(),
            },
            match r.mcp_alive {
                Some(true) => "up".into(),
                Some(false) if r.session_alive == Some(true) => "dormant".into(),
                Some(false) => "stopped".into(),
                None => "?".into(),
            },
            r.run.title.clone(),
        ]);
    }
    let text = if runs.is_empty() {
        "no active or waiting agent sessions".to_string()
    } else {
        t.to_string()
    };
    Ok(Out::Text(text, json!(runs)))
}

fn terminal_of(env: &Env, id: &str) -> Result<(String, Claim, Launcher)> {
    let (id, claim) = env.store()?.read(None, |c| {
        let id = c.resolve_id(id)?;
        let claim = c
            .session_claim(&id)?
            .filter(|claim| {
                claim
                    .terminal
                    .as_deref()
                    .is_some_and(|name| !name.trim().is_empty())
            })
            .ok_or_else(|| {
                Error::new(
                    Code::NotFound,
                    format!("{id} has no running session terminal"),
                )
            })?;
        Ok((id, claim))
    })?;
    let launcher = launcher::for_claim(env, claim.launcher.as_deref())?.ok_or_else(|| {
        Error::new(
            Code::InvalidState,
            format!("{id}: session has no named launcher profile"),
        )
        .with_issue(&id)
        .with_hint("this terminal cannot be controlled by trk; open it directly, or start a replacement session with `trk run` once the issue is open")
    })?;
    Ok((id, claim, launcher))
}

pub fn attach(env: &Env, id: &str) -> Result<Out> {
    let (id, claim, l) = terminal_of(env, id)?;
    let name = claim.terminal.as_deref().expect("filtered terminal");
    if !l.focus(name)?
        && let Some((false, _)) = l.operation(l.attach.as_deref(), name, None, true)?
    {
        return Err(Error::new(Code::Internal, format!("attach to {name} failed")).with_issue(&id));
    }
    Ok(Out::Silent(json!({"issue": id, "terminal": name})))
}

pub fn send(env: &Env, id: &str, text: &str) -> Result<Out> {
    let (id, claim, l) = terminal_of(env, id)?;
    let name = claim.terminal.expect("filtered terminal");
    let Some(tmpl) = l.send.as_deref().filter(|tmpl| !tmpl.is_empty()) else {
        return Ok(Out::Silent(
            json!({"issue": id, "terminal": name, "supported": false}),
        ));
    };
    let mut vars = BTreeMap::new();
    vars.insert("name", name.clone());
    vars.insert("text", delivery("trk-send", env.actor()?, text));
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
            .with_hint("ask the human to run `trk resume <ID> -m \"continuation instructions\"`; an eligible direct human chat reply may also resume a no-check wait through MCP"));
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
        if !ActorId::parse(&previous.actor)?.is_agent()
            || previous
                .terminal
                .as_deref()
                .is_none_or(|name| name.trim().is_empty())
        {
            return Err(unavailable());
        }
        let launcher =
            launcher::for_claim(env, previous.launcher.as_deref())?.ok_or_else(unavailable)?;
        let session = live_waiting_session(&previous, launcher).ok_or_else(unavailable)?;
        let claim = store.write(&actor, false, |c| {
            c.resume(&id, previous.id, revision, message)
        })?;
        let instructions = format!(
            "Human-authorized continuation for {id}. Your original session's claim and MCP tools have been restored with a fresh lease. \
             Continue using the issue's notes and these human instructions. End this new cycle with close, wait, or hand_back as appropriate. \
             This manual resume does not consume or reset the automatic failed-check wake budget.\n\n{message}"
        );
        if let Err(error) = session.send("human-resume", &actor, &instructions) {
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
    pub fn send(&self, source: &str, actor: &str, text: &str) -> Result<()> {
        let mut vars = BTreeMap::new();
        vars.insert("name", self.terminal.clone());
        vars.insert("text", delivery(source, actor, text));
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
    if claim
        .terminal
        .as_deref()
        .is_none_or(|name| name.trim().is_empty())
    {
        return Ok(None);
    }
    let Some(launcher) = launcher::for_claim(env, claim.launcher.as_deref())? else {
        return Ok(None);
    };
    Ok(live_waiting_session(&claim, launcher))
}

/// Best-effort UI update and notification to the session that left an issue waiting.
///
/// The waiting claim is historical by design: resolving the issue releases it,
/// so looking up the active claim could steer a replacement session instead.
pub fn notify_waiting_session(env: &Env, issue: &str, text: &str, closed: bool) -> Result<()> {
    let Some(claim) = waiting_claim(env, issue)? else {
        return Ok(());
    };
    let Some(name) = claim
        .terminal
        .as_deref()
        .filter(|name| !name.trim().is_empty())
    else {
        return Ok(());
    };
    let Some(launcher) = launcher::for_claim(env, claim.launcher.as_deref())? else {
        return Ok(());
    };
    // Renaming is independent of agent liveness and message delivery.
    if closed {
        let title = env.store()?.read(None, |c| Ok(c.issue(issue)?.title))?;
        let _ = launcher.rename(name, &format!("[closed] {title}"));
    }
    if let Some(session) = live_waiting_session(&claim, launcher) {
        session.send("check-notification", env.actor()?, text)?;
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
        if r.state != State::Claimed || claim_session_alive(env, &r.claim)? != Some(false) {
            continue;
        }
        let id = r.issue;
        let why = format!(
            "Session of {} ended: its terminal {} is gone. Claim released by trk reap.",
            r.claim.actor,
            r.claim.terminal.as_deref().unwrap_or(""),
        );
        store.write(&actor, false, |c| c.reap(&id, &why))?;
        reaped.push((id, r.claim.actor, why));
    }
    let text = if reaped.is_empty() {
        "nothing to reap".to_string()
    } else {
        reaped
            .iter()
            .map(|(issue, actor, _)| format!("released {issue} ({actor})"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let data = reaped
        .into_iter()
        .map(|(issue, actor, note)| json!({"issue": issue, "actor": actor, "note": note}))
        .collect();
    Ok(Out::Text(text, Value::Array(data)))
}
