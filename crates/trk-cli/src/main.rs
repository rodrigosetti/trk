//! `trk`: a local issue tracker and agent launcher.

mod check;
mod editor;
mod env;
mod herdr;
mod render;
mod run;
mod seed;
mod skill;

use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use trk_core::config::Level;
use trk_core::error::{Code, Error, Result, bad_args};
use trk_core::model::DEFAULT_PRIORITY;
use trk_core::ops::{ClaimOpts, Edit, NewIssue, WaitOpts};
use trk_core::view::With;

use crate::env::Env;
use crate::render::{Format, Out, Renderer};

#[derive(Parser)]
#[command(
    name = "trk",
    version = env!("TRK_VERSION"),
    about = "A local issue tracker and agent launcher",
    max_term_width = 100
)]
struct Cli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
pub struct Global {
    /// Output format (default: pretty on a TTY, json otherwise)
    #[arg(long, global = true, env = "TRK_FORMAT", value_parser = ["pretty", "json", "jsonl"])]
    format: Option<String>,
    /// Shorthand for --format json
    #[arg(long, global = true)]
    json: bool,
    /// Acting actor: human:<email> or agent:<harness>:<id>
    #[arg(long = "as", global = true, env = "TRK_ACTOR", value_name = "ACTOR")]
    pub actor: Option<String>,
    /// Project scope (default: resolved from the git repo)
    #[arg(long, global = true, env = "TRK_PROJECT")]
    pub project: Option<String>,
    /// Database file
    #[arg(long, global = true, env = "TRK_DB")]
    pub db: Option<PathBuf>,
    /// Config directory
    #[arg(long, global = true, env = "TRK_CONFIG")]
    pub config: Option<PathBuf>,
    /// Disable color
    #[arg(long, global = true)]
    no_color: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write the default config and create the database
    Init {
        /// Overwrite existing config files
        #[arg(long)]
        force: bool,
    },
    /// Query issues; `@view` runs a saved view
    List {
        query: Vec<String>,
        /// Columns (pretty) or keys (JSON)
        #[arg(long, value_delimiter = ',')]
        fields: Option<Vec<String>>,
        /// Sort by a field; prefix with - for descending
        #[arg(long, allow_hyphen_values = true)]
        sort: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Open issues whose blockers are all closed: `trk list 'ready …'`
    Ready {
        query: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        fields: Option<Vec<String>>,
        #[arg(long, allow_hyphen_values = true)]
        sort: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Full issue with derived state
    Show {
        #[arg(required = true)]
        ids: Vec<String>,
        /// Extra sections: notes,events,deps,claims (or all)
        #[arg(long, default_value = "deps,notes")]
        with: String,
    },
    /// Create an issue; opens $EDITOR when there is no title on a TTY
    Create {
        /// The title (same as -t)
        #[arg(value_name = "TITLE", conflicts_with = "title")]
        title_arg: Option<String>,
        #[arg(short = 't', long)]
        title: Option<String>,
        /// Description: text, @file, or @- for stdin
        #[arg(short = 'd', long)]
        description: Option<String>,
        /// P0 urgent, P1 high, P2 normal (default), P3 low
        #[arg(short = 'p', long)]
        priority: Option<String>,
        /// Agent definition for `trk run` (default: the configured default agent)
        #[arg(long)]
        agent: Option<String>,
        /// An issue that must close before this one is ready (repeatable)
        #[arg(long = "blocked-by", value_name = "ID")]
        blocked_by: Vec<String>,
        /// An issue that cannot be ready until this one closes (repeatable)
        #[arg(long, value_name = "ID")]
        blocks: Vec<String>,
        /// Use this id instead of allocating one (imports)
        #[arg(long, hide = true)]
        id: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Change title, description, priority or agent; $EDITOR with no options
    Edit {
        id: String,
        #[arg(short = 't', long)]
        title: Option<String>,
        /// Description: text, @file, or @- for stdin
        #[arg(short = 'd', long)]
        description: Option<String>,
        #[arg(short = 'p', long)]
        priority: Option<String>,
        /// Agent definition; an empty value clears it
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Claim a ready issue: open → claimed
    Claim {
        id: String,
        /// Lease, e.g. 30m (agent claims default to the configured lease)
        #[arg(long)]
        lease: Option<String>,
        #[arg(long)]
        worktree: Option<PathBuf>,
        #[arg(long)]
        steal: bool,
    },
    /// Release the claim: claimed → open
    Unclaim {
        id: String,
        #[arg(long)]
        force: bool,
        /// Note recorded with the release
        #[arg(short = 'm', long)]
        message: Option<String>,
    },
    /// Extend the lease
    Renew {
        id: String,
        #[arg(long)]
        lease: Option<String>,
    },
    /// Close an open, claimed or waiting issue
    Close {
        id: String,
        /// done or abandoned
        #[arg(short = 'r', long, default_value = "done")]
        reason: String,
        /// Note recorded with the close: the outcome
        #[arg(short = 'm', long)]
        message: Option<String>,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// End work without closing: claimed → waiting, until a check passes or a human decides
    #[command(
        after_help = "The check runs later, by `trk check`, with `sh -c` in the claim's worktree.\n\
        Exit 0 closes the issue as done; exit 75 means not yet, check again after --every;\n\
        any other exit returns the issue to open with the output as a note.\n\
        Without --check the issue waits for `trk close` or `trk reopen`.\n\
        On an issue that is already waiting, this replaces its check, interval and reason."
    )]
    Wait {
        id: String,
        /// Shell command that decides the issue's fate (text, @file, or @-)
        #[arg(short = 'c', long)]
        check: Option<String>,
        /// Time between checks, e.g. 2m, 1h (default: [check] interval, 5m)
        #[arg(long, value_name = "DURATION")]
        every: Option<String>,
        /// What the issue is waiting for
        #[arg(short = 'r', long)]
        reason: Option<String>,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Return a waiting or closed issue to open
    Reopen {
        id: String,
        /// Note recorded with the reopen
        #[arg(short = 'm', long)]
        message: Option<String>,
    },
    /// Run the checks of waiting issues that are due, one at a time
    Check {
        /// Run only this issue's check, due or not
        id: Option<String>,
        /// Run every check, due or not
        #[arg(long)]
        all: bool,
        /// Keep running, pausing between passes (default 30s)
        #[arg(long, value_name = "PAUSE", num_args = 0..=1)]
        watch: Option<Option<String>>,
        /// Show what would run, without running it
        #[arg(long)]
        dry_run: bool,
    },
    /// Append a note (text, or - / nothing for stdin)
    Note {
        id: String,
        text: Option<String>,
        #[arg(long)]
        amends: Option<i64>,
        /// Also queue it for the agent's inbox
        #[arg(long)]
        to_agent: bool,
    },
    /// Add or remove a blocks edge: BLOCKER blocks BLOCKED
    Dep {
        #[command(subcommand)]
        cmd: DepCmd,
    },
    /// Timeline: events and notes
    Log { id: String },
    /// Start an agent session on the issue through the launcher
    Run {
        id: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        harness: Option<String>,
        /// Run the harness in this terminal
        #[arg(long)]
        fg: bool,
        /// Claim, write files, and print the launch spec instead of starting
        #[arg(long)]
        print: bool,
        /// Print the launch spec without claiming or writing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Active agent claims with agent, harness, terminal and lease
    Runs,
    /// Attach to the session's terminal
    Attach { id: String },
    /// Type text into the session's terminal
    Send { id: String, text: String },
    /// Release claims whose session has ended, with a note
    Reap,
    /// The per-issue MCP server (started by the harness)
    Mcp {
        #[arg(long)]
        issue: String,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        agent: String,
    },
    /// Inspect agent definitions
    Agent {
        #[command(subcommand)]
        cmd: AgentCmd,
    },
    /// Register projects
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Register repositories
    Repo {
        #[command(subcommand)]
        cmd: RepoCmd,
    },
    /// Install the agent skill that teaches agent sessions to use the trk CLI
    Skill {
        #[command(subcommand)]
        cmd: SkillCmd,
    },
    /// Resolved actor, project, config and database
    Whoami,
    /// Consistent copy via VACUUM INTO
    Backup { path: PathBuf },
}

#[derive(Subcommand)]
enum DepCmd {
    /// BLOCKED is not ready until BLOCKER is closed
    Add {
        blocker: String,
        blocked: String,
    },
    Rm {
        blocker: String,
        blocked: String,
    },
}

#[derive(Subcommand)]
enum AgentCmd {
    List,
    /// What a session would receive: instructions, briefing, tools, MCP servers
    Preview {
        agent: String,
        /// Render for this issue instead of a sample
        #[arg(long)]
        issue: Option<String>,
    },
    /// Check agent definitions, their templates and harness profiles
    Check,
}

#[derive(Args, Clone)]
struct SkillScope {
    /// ~/.agents/skills/trk (and ~/.claude/skills/trk) instead of this repository
    #[arg(long, conflicts_with = "dir")]
    global: bool,
    /// Another skills directory; the skill goes in DIR/trk
    #[arg(long, value_name = "DIR")]
    dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum SkillCmd {
    /// Install or refresh the skill (default: in this repository, kept out of git)
    Install {
        #[command(flatten)]
        scope: SkillScope,
    },
    /// Remove an installed skill
    Uninstall {
        #[command(flatten)]
        scope: SkillScope,
    },
    /// Where the skill is installed and whether it matches this binary
    Status {
        #[command(flatten)]
        scope: SkillScope,
    },
}

impl SkillScope {
    fn scope(&self) -> skill::Scope {
        if self.dir.is_some() {
            skill::Scope::Dir
        } else if self.global {
            skill::Scope::Global
        } else {
            skill::Scope::Project
        }
    }
}

#[derive(Subcommand)]
enum ProjectCmd {
    Add {
        id: String,
        #[arg(long)]
        name: Option<String>,
    },
    List,
}

#[derive(Subcommand)]
enum RepoCmd {
    /// Register a repo (default: the current git checkout)
    Add {
        /// Remote URL or local path
        target: Option<String>,
    },
    List,
}

fn main() {
    let cli = Cli::parse();
    let g = cli.global.clone();
    let format = if g.json {
        Format::Json
    } else {
        match g.format.as_deref() {
            Some("json") => Format::Json,
            Some("jsonl") => Format::Jsonl,
            Some(_) => Format::Pretty,
            None => {
                if std::io::stdout().is_terminal() {
                    Format::Pretty
                } else {
                    Format::Json
                }
            }
        }
    };
    let color = format == Format::Pretty
        && !g.no_color
        && std::env::var_os("NO_COLOR").is_none()
        && std::io::stdout().is_terminal();
    let r = Renderer { format, color };
    if let Cmd::Mcp {
        issue,
        actor,
        agent,
    } = &cli.cmd
    {
        // The MCP server owns stdout; errors go to stderr.
        let code = match mcp(&g, issue, actor, agent) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("trk mcp: {e:#}");
                1
            }
        };
        std::process::exit(code);
    }
    match dispatch(&g, cli.cmd) {
        Ok(out) => r.emit(out),
        Err(e) => {
            r.error(&e);
            std::process::exit(e.exit_code());
        }
    }
}

fn mcp(g: &Global, issue: &str, actor: &str, agent: &str) -> anyhow::Result<()> {
    let env = Env::new(g)?;
    let now = std::env::var("TRK_NOW").ok().and_then(|s| s.parse().ok());
    trk_mcp::serve(trk_mcp::McpArgs {
        db: env.db.clone(),
        config: env.config.clone(),
        opts: trk_core::StoreOpts {
            now,
            agent_lease: env.agent_lease()?,
            current_repo: None,
            check_interval: trk_core::config::parse_duration(&env.user.check.interval)?,
        },
        issue: issue.to_string(),
        actor: actor.to_string(),
        agent: agent.to_string(),
    })
}

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

fn dispatch(g: &Global, cmd: Cmd) -> Result<Out> {
    let env = Env::new(g)?;
    match cmd {
        Cmd::Init { force } => init(&env, force),
        Cmd::List {
            query,
            fields,
            sort,
            limit,
        } => list(&env, &query.join(" "), fields, sort, limit, false),
        Cmd::Ready {
            query,
            fields,
            sort,
            limit,
        } => list(&env, &query.join(" "), fields, sort, limit, true),
        Cmd::Show { ids, with } => {
            let store = env.store()?;
            let w = With::parse(&with);
            let mut items = vec![];
            for id in &ids {
                items.push(store.read(env.actor.as_deref(), |c| c.show(id, w))?);
            }
            if items.len() == 1 {
                Ok(Out::Issue(items.pop().unwrap()))
            } else {
                Ok(Out::IssueList(items))
            }
        }
        Cmd::Create {
            title_arg,
            title,
            description,
            priority,
            agent,
            blocked_by,
            blocks,
            id,
            dry_run,
        } => create(
            &env,
            NewIssue {
                id,
                title: title.or(title_arg).unwrap_or_default(),
                description: description
                    .as_deref()
                    .map(read_value)
                    .transpose()?
                    .unwrap_or_default(),
                priority,
                agent,
                blocked_by,
                blocks,
                ..Default::default()
            },
            dry_run,
        ),
        Cmd::Edit {
            id,
            title,
            description,
            priority,
            agent,
            dry_run,
        } => edit(
            &env,
            id,
            Edit {
                title,
                description: description.as_deref().map(read_value).transpose()?,
                priority,
                agent: agent.map(|a| Some(a).filter(|a| !a.trim().is_empty())),
            },
            dry_run,
        ),
        Cmd::Claim {
            id,
            lease,
            worktree,
            steal,
        } => {
            let actor = env.actor()?.to_string();
            let lease = lease
                .map(|l| trk_core::config::parse_duration(&l))
                .transpose()?;
            let explicit = worktree.map(|p| {
                std::fs::canonicalize(&p)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .to_string()
            });
            with_open(&env, |s| {
                s.write(&actor, false, |c| {
                    let id = c.resolve_id(&id)?;
                    // Record where the work happens: the given worktree, or the
                    // working directory when it is a checkout of the issue's repo.
                    let worktree = match explicit {
                        Some(w) => Some(w),
                        None => {
                            let issue = c.issue(&id)?;
                            let here = env
                                .git
                                .as_ref()
                                .map(|g| c.find_repo(g.remote.as_deref(), Some(&g.toplevel)))
                                .transpose()?
                                .flatten();
                            match (&env.git, here) {
                                (Some(g), Some(r))
                                    if issue.repo_id == Some(r.id)
                                        || (issue.repo_id.is_none()
                                            && r.project == issue.project) =>
                                {
                                    Some(g.toplevel.clone())
                                }
                                _ => None,
                            }
                        }
                    };
                    c.claim(
                        &id,
                        ClaimOpts {
                            lease,
                            steal,
                            worktree,
                            host: env::hostname(),
                            ..Default::default()
                        },
                    )
                    .map(|_| ())
                })?;
                show_one(&env, s, &id)
            })
        }
        Cmd::Unclaim { id, force, message } => {
            let actor = env.actor()?.to_string();
            with_open(&env, |s| {
                s.write(&actor, false, |c| c.unclaim(&id, force, message.as_deref()))?;
                show_one(&env, s, &id)
            })
        }
        Cmd::Renew { id, lease } => {
            let actor = env.actor()?.to_string();
            let lease = lease
                .map(|l| trk_core::config::parse_duration(&l))
                .transpose()?;
            with_open(&env, |s| {
                let c = s.write(&actor, false, |c| c.renew(&id, lease))?;
                Ok(Out::Text(
                    format!(
                        "{} lease until {}",
                        c.issue_id,
                        c.lease_until.clone().unwrap_or_else(|| "(no lease)".into())
                    ),
                    serde_json::to_value(&c)?,
                ))
            })
        }
        Cmd::Close {
            id,
            reason,
            message,
            force,
            dry_run,
        } => {
            let actor = env.actor()?.to_string();
            with_open(&env, |s| {
                s.write(&actor, dry_run, |c| {
                    c.close(&id, &reason, message.as_deref(), force)?;
                    c.show(&id, SHOWN)
                })
                .map(Out::Issue)
            })
        }
        Cmd::Wait {
            id,
            check,
            every,
            reason,
            force,
            dry_run,
        } => {
            let actor = env.actor()?.to_string();
            let opts = WaitOpts {
                check: check.as_deref().map(read_value).transpose()?,
                interval: every
                    .map(|e| trk_core::config::parse_duration(&e))
                    .transpose()?,
                reason,
                force,
            };
            with_open(&env, |s| {
                s.write(&actor, dry_run, |c| {
                    c.wait(&id, opts)?;
                    c.show(&id, SHOWN)
                })
                .map(Out::Issue)
            })
        }
        Cmd::Reopen { id, message } => {
            let actor = env.actor()?.to_string();
            with_open(&env, |s| {
                s.write(&actor, false, |c| c.reopen(&id, message.as_deref()))?;
                show_one(&env, s, &id)
            })
        }
        Cmd::Check {
            id,
            all,
            watch,
            dry_run,
        } => check::check(
            &env,
            check::CheckArgs {
                id,
                all,
                watch,
                dry_run,
            },
        ),
        Cmd::Note {
            id,
            text,
            amends,
            to_agent,
        } => {
            let actor = env.actor()?.to_string();
            let body = match text.as_deref() {
                None | Some("-") => read_value("@-")?,
                Some(t) => t.to_string(),
            };
            with_open(&env, |s| {
                let seq = s.write(&actor, false, |c| c.note(&id, &body, amends, to_agent))?;
                Ok(Out::Text(
                    format!(
                        "note #{seq} added{}",
                        if to_agent {
                            " and queued for the agent"
                        } else {
                            ""
                        }
                    ),
                    json!({"seq": seq, "to_agent": to_agent}),
                ))
            })
        }
        Cmd::Dep { cmd } => {
            let actor = env.actor()?.to_string();
            with_open(&env, |s| match cmd {
                DepCmd::Add { blocker, blocked } => {
                    let (added, blocker, blocked) = s.write(&actor, false, |c| {
                        let (a, b) = (c.resolve_id(&blocker)?, c.resolve_id(&blocked)?);
                        Ok((c.dep_add(&a, &b)?, a, b))
                    })?;
                    Ok(Out::Text(
                        format!(
                            "{blocker} blocks {blocked}{}",
                            if added { "" } else { " (already present)" }
                        ),
                        json!({"blocker": blocker, "blocked": blocked, "added": added}),
                    ))
                }
                DepCmd::Rm { blocker, blocked } => {
                    let (blocker, blocked) = s.write(&actor, false, |c| {
                        let (a, b) = (c.resolve_id(&blocker)?, c.resolve_id(&blocked)?);
                        c.dep_rm(&a, &b)?;
                        Ok((a, b))
                    })?;
                    Ok(Out::Text(
                        format!("{blocker} no longer blocks {blocked}"),
                        json!({"blocker": blocker, "blocked": blocked}),
                    ))
                }
            })
        }
        Cmd::Log { id } => {
            let store = env.store()?;
            let (id, events) = store.read(None, |c| {
                let id = c.resolve_id(&id)?;
                Ok((id.clone(), c.log(&id)?))
            })?;
            Ok(Out::Log {
                issue: id,
                events: events
                    .into_iter()
                    .map(|e| serde_json::to_value(e).unwrap_or_default())
                    .collect(),
            })
        }
        Cmd::Run {
            id,
            agent,
            harness,
            fg,
            print,
            dry_run,
        } => run::run(
            &env,
            run::RunArgs {
                id,
                agent,
                harness,
                fg,
                print,
                dry_run,
            },
        ),
        Cmd::Runs => run::runs(&env),
        Cmd::Attach { id } => run::attach(&env, &id),
        Cmd::Send { id, text } => run::send(&env, &id, &text),
        Cmd::Reap => run::reap(&env),
        Cmd::Mcp { .. } => unreachable!(),
        Cmd::Agent { cmd } => agent_cmd(&env, cmd),
        Cmd::Project { cmd } => match cmd {
            ProjectCmd::Add { id, name } => {
                let actor = env.actor()?.to_string();
                with_open(&env, |s| {
                    let p = s.write(&actor, false, |c| {
                        c.project_add(&id, name.as_deref().unwrap_or(&id))
                    })?;
                    Ok(Out::Text(
                        format!("project {} added", p.id),
                        serde_json::to_value(p)?,
                    ))
                })
            }
            ProjectCmd::List => {
                let store = env.store()?;
                let ps = store.read(None, |c| c.projects())?;
                let text = ps
                    .iter()
                    .map(|p| format!("{}  {}", p.id, p.name))
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(Out::Text(
                    if text.is_empty() {
                        "no projects".into()
                    } else {
                        text
                    },
                    serde_json::to_value(ps)?,
                ))
            }
        },
        Cmd::Repo { cmd } => match cmd {
            RepoCmd::Add { target } => repo_add(&env, target),
            RepoCmd::List => {
                let store = env.store()?;
                let rs = store.read(None, |c| c.repos())?;
                let text = rs
                    .iter()
                    .map(|r| {
                        format!(
                            "{}  {}  {}",
                            r.project,
                            r.remote_url.as_deref().unwrap_or("-"),
                            r.local_path.as_deref().unwrap_or("-")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(Out::Text(
                    if text.is_empty() {
                        "no repos".into()
                    } else {
                        text
                    },
                    serde_json::to_value(rs)?,
                ))
            }
        },
        Cmd::Skill { cmd } => match cmd {
            SkillCmd::Install { scope } => {
                skill::install(&env, scope.scope(), scope.dir.as_deref())
            }
            SkillCmd::Uninstall { scope } => {
                skill::uninstall(&env, scope.scope(), scope.dir.as_deref())
            }
            SkillCmd::Status { scope } => skill::status(&env, scope.global, scope.dir.as_deref()),
        },
        Cmd::Whoami => whoami(&env),
        Cmd::Backup { path } => {
            let store = env.store()?;
            store.backup(&path)?;
            Ok(Out::Text(
                format!("backup written to {}", path.display()),
                json!({"path": path}),
            ))
        }
    }
}

fn init(env: &Env, force: bool) -> Result<Out> {
    let dir = &env.config.path;
    std::fs::create_dir_all(dir)?;
    let mut written = vec![];
    let mut kept = vec![];
    let email = env
        .actor
        .as_deref()
        .and_then(|a| a.strip_prefix("human:"))
        .map(String::from);
    let config_toml = seed::CONFIG_TOML.replace(
        "{{EMAIL_LINE}}",
        &match &email {
            Some(e) => format!("email = \"{e}\""),
            None => "# email = \"you@example.com\"".to_string(),
        },
    );
    let mut files: Vec<(&str, String)> = vec![("config.toml", config_toml)];
    files.extend(seed::FILES.iter().map(|(p, c)| (*p, c.to_string())));
    for (rel, content) in files {
        let p = dir.join(rel);
        if p.exists() && !force {
            kept.push(rel.to_string());
            continue;
        }
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, content)?;
        written.push(rel.to_string());
    }
    let stale: Vec<String> = seed::STALE
        .iter()
        .filter(|f| dir.join(f).exists())
        .map(|f| f.to_string())
        .collect();
    // Opening the store creates the database.
    env.store()?;
    let mut text = format!(
        "config: {}\n  wrote {} files{}\ndatabase: {}",
        dir.display(),
        written.len(),
        if kept.is_empty() {
            String::new()
        } else {
            format!(", kept {} existing (use --force to overwrite)", kept.len())
        },
        env.db.display(),
    );
    if !stale.is_empty() {
        text.push_str(&format!(
            "\n\nNo longer used, safe to delete from the config directory:\n  {}",
            stale.join("\n  ")
        ));
    }
    text.push_str(
        "\n\nNext:\n  trk project add <id> --name \"…\"\n  trk repo add --project <id>      (inside a git checkout)\n  trk create \"…\" -d \"…\"\n  trk check --watch                (in a spare terminal: runs waiting checks)",
    );
    Ok(Out::Text(
        text,
        json!({"config": dir, "db": env.db, "written": written, "kept": kept, "stale": stale}),
    ))
}

fn list(
    env: &Env,
    q: &str,
    fields: Option<Vec<String>>,
    sort: Option<String>,
    limit: Option<usize>,
    ready: bool,
) -> Result<Out> {
    let mut q = q.trim().to_string();
    let mut fields = fields;
    let mut sort = sort;
    let mut limit = limit;
    if let Some(name) = q.strip_prefix('@') {
        let (name, rest) = name.split_once(' ').unwrap_or((name, ""));
        let v = env.user.views.get(name).ok_or_else(|| {
            Error::new(Code::NotFound, format!("no view '@{name}'")).with_hint(format!(
                "views: {}",
                env.user
                    .views
                    .keys()
                    .map(|k| format!("@{k}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ))
        })?;
        q = format!("{} {rest}", v.query);
        fields = fields.or_else(|| v.fields.clone());
        sort = sort.or_else(|| v.sort.clone());
        limit = limit.or(v.limit);
    }
    if ready {
        q = format!("ready {q}");
    }
    let store = env.store()?;
    let items = store.read(env.actor.as_deref(), |c| {
        c.list(
            q.trim(),
            env.explicit_project.as_deref(),
            sort.as_deref(),
            limit,
        )
    })?;
    Ok(Out::Issues { items, fields })
}

fn create(env: &Env, mut new: NewIssue, dry_run: bool) -> Result<Out> {
    let actor = env.actor()?.to_string();
    let mut store = env.store()?;
    new.project = env.project(&store)?;
    let mut attempt = |new: NewIssue| -> Result<Value> {
        store.write(&actor, dry_run, |c| {
            let id = c.create(new)?;
            c.show(&id, SHOWN)
        })
    };
    if new.title.trim().is_empty() && editor::is_tty() && !dry_run {
        let doc = editor::Doc {
            title: String::new(),
            priority: new
                .priority
                .clone()
                .unwrap_or_else(|| DEFAULT_PRIORITY.to_string()),
            agent: new.agent.clone().unwrap_or_default(),
            description: if new.description.is_empty() {
                "<!-- What is needed and why, with enough context to start cold. -->\n".into()
            } else {
                new.description.clone()
            },
        };
        let v = editor::edit_loop(&doc, |d| {
            let description = d.description.trim();
            let placeholder = description.starts_with("<!--") && description.ends_with("-->");
            attempt(NewIssue {
                title: d.title,
                priority: Some(d.priority).filter(|p| !p.trim().is_empty()),
                agent: Some(d.agent).filter(|a| !a.trim().is_empty()),
                description: if placeholder {
                    String::new()
                } else {
                    d.description
                },
                ..new.clone()
            })
        })?;
        return Ok(Out::Issue(v));
    }
    Ok(Out::Issue(attempt(new)?))
}

fn edit(env: &Env, id: String, e: Edit, dry_run: bool) -> Result<Out> {
    let actor = env.actor()?.to_string();
    let mut store = env.store()?;
    let nothing =
        e.title.is_none() && e.description.is_none() && e.priority.is_none() && e.agent.is_none();
    if !nothing {
        return store
            .write(&actor, dry_run, |c| {
                let id = c.resolve_id(&id)?;
                c.edit(&id, e)?;
                c.show(&id, SHOWN)
            })
            .map(Out::Issue);
    }
    if !editor::is_tty() {
        return Err(bad_args("nothing to change")
            .with_hint("pass -t TITLE, -d DESCRIPTION, -p PRIORITY or --agent NAME"));
    }
    let cur = store.read(None, |c| {
        let id = c.resolve_id(&id)?;
        c.issue(&id)
    })?;
    let doc = editor::Doc {
        title: cur.title.clone(),
        priority: cur.priority.clone(),
        agent: cur.agent.clone().unwrap_or_default(),
        description: cur.description.clone(),
    };
    let v = editor::edit_loop(&doc, |d| {
        store.write(&actor, dry_run, |c| {
            c.edit(
                &cur.id,
                Edit {
                    title: Some(d.title),
                    description: Some(d.description),
                    priority: Some(d.priority),
                    agent: Some(Some(d.agent).filter(|a| !a.trim().is_empty())),
                },
            )?;
            c.show(&cur.id, SHOWN)
        })
    })?;
    Ok(Out::Issue(v))
}

fn repo_add(env: &Env, target: Option<String>) -> Result<Out> {
    let actor = env.actor()?.to_string();
    let project = env.explicit_project.clone().ok_or_else(|| {
        bad_args("which project does the repo belong to?")
            .with_hint("trk repo add --project <id> [URL|PATH]")
    })?;
    let (remote, path) = match target {
        None => {
            let g = env.git.as_ref().ok_or_else(|| {
                bad_args("not inside a git repository").with_hint("pass a remote URL or a path")
            })?;
            (g.remote.clone(), Some(g.toplevel.clone()))
        }
        Some(t) if std::path::Path::new(&t).exists() => {
            let gi = env::git_info(std::path::Path::new(&t));
            match gi {
                Some(g) => (g.remote, Some(g.toplevel)),
                None => (
                    None,
                    Some(std::fs::canonicalize(&t)?.to_string_lossy().to_string()),
                ),
            }
        }
        Some(t) => (Some(t), None),
    };
    with_open(env, |s| {
        let r = s.write(&actor, false, |c| {
            c.repo_add(&project, remote.as_deref(), path.as_deref())
        })?;
        Ok(Out::Text(
            format!(
                "repo {} registered to project {}",
                r.remote_url
                    .clone()
                    .or(r.local_path.clone())
                    .unwrap_or_default(),
                r.project
            ),
            serde_json::to_value(r)?,
        ))
    })
}

fn whoami(env: &Env) -> Result<Out> {
    let store = env.store()?;
    let project = env.project(&store).ok();
    let repo = env.current_repo(&store)?;
    let (counts, lock) = store.read(None, |c| Ok((c.counts()?, c.check_lock()?)))?;
    let data = json!({
        "version": env!("TRK_VERSION"),
        "actor": env.actor,
        "project": project,
        "repo": repo,
        "config": env.config.path,
        "config_commit": env.config.git_commit(),
        "db": env.db,
        "default_agent": env.default_agent(),
        "issues": counts,
        "check_running": lock.map(|(pid, host, started)| json!({"pid": pid, "host": host, "started_at": started})),
    });
    Ok(Out::Data(data))
}

// ----------------------------------------------------------------------
// Agents
// ----------------------------------------------------------------------

fn diag_text(d: &[trk_core::config::Diagnostic]) -> String {
    d.iter()
        .map(|x| {
            format!(
                "{:<7} {}{}",
                match x.level {
                    Level::Error => "error",
                    Level::Warning => "warning",
                },
                x.file
                    .as_ref()
                    .map(|f| format!("{f}: "))
                    .unwrap_or_default(),
                x.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn agent_cmd(env: &Env, cmd: AgentCmd) -> Result<Out> {
    match cmd {
        AgentCmd::List => {
            let agents = trk_agent::load_agents(&env.config)?;
            let default = env.default_agent();
            let mut items = vec![];
            let mut text = String::new();
            for a in agents.values() {
                let tools: Vec<String> = trk_agent::agent_tools(a)
                    .into_iter()
                    .map(|t| t.name)
                    .collect();
                text.push_str(&format!(
                    "{:<12} {}{}\n             harness: {}  builtin_tools: {}\n             tools: {}\n",
                    a.name,
                    a.description.clone().unwrap_or_default(),
                    if a.name == default { "  (default)" } else { "" },
                    a.harness.prefer.join(" > "),
                    a.harness.builtin_tools,
                    tools.join(", "),
                ));
                items.push(json!({"name": a.name, "description": a.description,
                                  "default": a.name == default, "tools": tools,
                                  "harness": a.harness.prefer, "builtin_tools": a.harness.builtin_tools}));
            }
            if text.is_empty() {
                text = "no agents: run `trk init`".into();
            }
            Ok(Out::Text(text, Value::Array(items)))
        }
        AgentCmd::Check => {
            let d = trk_agent::check::check_agents(&env.config, &env.user);
            let errors = d.iter().filter(|x| x.level == Level::Error).count();
            let data = json!({"ok": errors == 0, "diagnostics": d});
            if errors > 0 {
                return Err(Error::new(
                    Code::ConfigInvalid,
                    format!("{errors} problem(s) in the agent configuration"),
                )
                .with_detail(
                    "problems",
                    json!(
                        d.iter()
                            .map(|x| format!(
                                "{}{}",
                                x.file
                                    .as_ref()
                                    .map(|f| format!("{f}: "))
                                    .unwrap_or_default(),
                                x.message
                            ))
                            .collect::<Vec<_>>()
                    ),
                ));
            }
            let text = if d.is_empty() {
                "agents, templates and harness profiles are fine".to_string()
            } else {
                diag_text(&d)
            };
            Ok(Out::Text(text, data))
        }
        AgentCmd::Preview { agent, issue } => {
            let a = trk_agent::load_agent(&env.config, &agent)?;
            let issue_json = match &issue {
                Some(id) => {
                    let store = env.store()?;
                    store.read(env.actor.as_deref(), |c| {
                        c.show(
                            id,
                            With {
                                deps: true,
                                notes: true,
                                ..Default::default()
                            },
                        )
                    })?
                }
                None => trk_agent::check::sample_issue(),
            };
            let tools = trk_agent::agent_tools(&a);
            let input = trk_agent::prompt::PromptInput {
                issue: &issue_json,
                agent: &a,
                tools: &tools,
                worktree: issue_json["claim"]["worktree"].as_str(),
                actor: "agent:<harness>:<session>",
                harness: None,
            };
            let instructions = trk_agent::prompt::instructions(&env.config, &input)?;
            let briefing = trk_agent::prompt::briefing(&env.config, &input, None)?;
            let tool_json: Vec<Value> = tools
                .iter()
                .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.input_schema}))
                .collect();
            let others: Vec<Value> = a
                .mcp
                .iter()
                .map(|m| json!({"name": m.name, "command": m.command, "args": m.args}))
                .collect();
            let mut text = format!(
                "== instructions (MCP server) ==\n{instructions}\n\n== briefing (initial prompt) ==\n{briefing}\n\n== trk tools ==\n",
            );
            for tl in &tools {
                let req: Vec<String> = tl.input_schema["required"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect();
                text.push_str(&format!(
                    "  {}({})\n      {}\n",
                    tl.name,
                    req.join(", "),
                    tl.description
                ));
            }
            if !others.is_empty() {
                text.push_str("\n== other MCP servers ==\n");
                for m in &a.mcp {
                    text.push_str(&format!(
                        "  {}: {} {}\n",
                        m.name,
                        m.command,
                        m.args.join(" ")
                    ));
                }
            }
            Ok(Out::Text(
                text,
                json!({"agent": a.name, "instructions": instructions, "briefing": briefing, "tools": tool_json, "mcp": others}),
            ))
        }
    }
}
