mod agents;
mod claims;
mod create;
mod edit;
mod epic;
mod herdr;
mod history;
mod install;
mod list;
mod mcp;
mod project;
mod relations;
mod show;
mod state;

use std::io::Read;

use trk_core::error::{Result, bad_args};
use trk_core::view::With;

use crate::env::Env;
use crate::render::{Out, Renderer};
use crate::{AgentCmd, Cmd, Global, HerdrCmd, check, dashboard, gc, herdr_ui, run};

pub(super) use mcp::serve as mcp;

/// A value given as text, `@file`, or `@-` for stdin.
fn read_value(v: &str) -> Result<String> {
    if let Some(p) = v.strip_prefix('@') {
        if p == "-" {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            return Ok(s.trim_end_matches('\n').to_string());
        }
        return std::fs::read_to_string(p)
            .map(|s| s.trim_end_matches('\n').to_string())
            .map_err(|e| bad_args(format!("cannot read {p}: {e}")));
    }
    Ok(v.to_string())
}

fn with_open<T>(env: &Env, f: impl FnOnce(&mut trk_core::Store) -> Result<T>) -> Result<T> {
    let mut s = env.store()?;
    f(&mut s)
}

const SHOWN: With = With {
    deps: true,
    notes: false,
    events: false,
    claims: false,
};

fn show_one(env: &Env, store: &trk_core::Store, id: &str) -> Result<Out> {
    let actor = env.actor.clone();
    Ok(Out::Issue(
        store.read(actor.as_deref(), |c| c.show(id, SHOWN))?,
    ))
}

pub(super) fn dispatch(
    g: &Global,
    cmd: Cmd,
    completed: Vec<check::CheckView>,
    r: &Renderer,
) -> Result<Out> {
    if matches!(cmd, Cmd::Install { .. }) {
        return install::run(g, cmd);
    }
    // The mod is machine-wide: installing and removing it need no project.
    let cmd = match cmd {
        Cmd::Herdr {
            cmd: HerdrCmd::Install { if_installed },
        } => {
            return herdr_ui::install(
                &herdr_ui::app_home(),
                herdr_ui::InstallOpts { if_installed },
            );
        }
        Cmd::Herdr {
            cmd: HerdrCmd::Uninstall { force },
        } => return herdr_ui::uninstall(&herdr_ui::app_home(), force),
        cmd => cmd,
    };
    let env = Env::new(g)?;
    match cmd {
        Cmd::Install { .. } => unreachable!(),
        Cmd::Herdr {
            cmd: HerdrCmd::Open { split, space },
        } => herdr::open(&env, split, space),
        Cmd::Herdr { .. } => unreachable!(),
        Cmd::List { .. } | Cmd::Ready { .. } => list::run(&env, cmd),
        Cmd::Show { .. } => show::run(&env, cmd),
        Cmd::Create { .. } => create::run(&env, cmd),
        Cmd::Edit { .. } => edit::run(&env, cmd),
        Cmd::Epic { cmd } => epic::run(&env, cmd, r),
        Cmd::Claim { .. } | Cmd::Unclaim { .. } | Cmd::Renew { .. } => claims::run(&env, cmd),
        Cmd::Close { .. } | Cmd::Wait { .. } | Cmd::Reopen { .. } => state::run(&env, cmd),
        Cmd::Note { .. } | Cmd::Log { .. } => history::run(&env, cmd),
        Cmd::Dep { .. } | Cmd::Link { .. } | Cmd::Unlink { .. } => relations::run(&env, cmd),
        Cmd::Resume { id, message } => run::resume(&env, &id, message.as_deref().unwrap_or("")),
        Cmd::Check {
            id,
            all,
            watch,
            dry_run,
            no_gc,
        } => check::check(
            &env,
            check::CheckArgs {
                id,
                all,
                watch,
                dry_run,
                no_gc,
            },
            completed,
            r,
        ),
        Cmd::Gc {
            ids,
            dry_run,
            abandoned,
            older_than,
            legacy_hc,
        } => gc::run(
            &env,
            ids,
            dry_run,
            abandoned,
            older_than.as_deref(),
            legacy_hc,
        ),
        Cmd::Run {
            id,
            agent,
            harness,
            model,
            fg,
            print,
            dry_run,
        } => run::run(
            &env,
            run::RunArgs {
                id,
                agent,
                harness,
                model,
                fg,
                print,
                dry_run,
            },
        ),
        Cmd::Runs => run::runs(&env),
        Cmd::Dashboard => dashboard::dashboard(
            &env,
            g.json || g.format.as_deref().is_some_and(|f| f != "pretty"),
        ),
        Cmd::Attach { id } => run::attach(&env, &id),
        Cmd::Send { id, text } => run::send(&env, &id, &text),
        Cmd::Reap => run::reap(&env),
        Cmd::Mcp { .. } => unreachable!(),
        Cmd::Agent { cmd } => agents::agent_cmd(&env, cmd),
        Cmd::Agents => agents::agent_cmd(&env, AgentCmd::List),
        Cmd::Whoami => project::whoami(&env),
        Cmd::Backup { path } => project::backup(&env, &path),
    }
}
