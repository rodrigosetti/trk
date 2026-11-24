//! `trk`: a local issue tracker and agent launcher.

mod check;
mod dashboard;
mod editor;
mod env;
mod gc;
mod herdr;
mod herdr_ui;
mod launcher;
mod render;
mod run;
mod skill;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use trk_core::config::Level;
use trk_core::error::{Code, Error, Result, bad_args};
use trk_core::model::DEFAULT_PRIORITY;
use trk_core::ops::{ClaimOpts, Edit, NewIssue, WaitOpts};
use trk_core::seed;
use trk_core::view::With;

use crate::env::Env;
use crate::render::{Format, Out, Renderer};

#[derive(Parser)]
#[command(
    name = "trk",
    version = env!("TRK_VERSION"),
    about = "A local issue tracker and agent launcher",
    after_help = "Manual: https://rodrigosetti.github.io/trk/",
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
    /// The project's .trk directory (default: found from the working directory)
    #[arg(long, global = true, env = "TRK_DIR", value_name = "DIR")]
    pub dir: Option<PathBuf>,
    /// Disable color
    #[arg(long, global = true)]
    no_color: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Set trk up in this project: .trk/ with the database and default config, and the agent skill
    #[command(
        after_help = "Run it in the project's root directory (in a git repository, the root of the\n\
        primary checkout is used). Everything goes in .trk/, which starts out ignored by git.\n\
        Safe to run again: it refreshes the agent skill and upgrades the database, and keeps\n\
        existing config files unless --force is given.\n\
        It also refreshes the Herdr mod when that is installed (see `trk herdr`)."
    )]
    Install {
        /// Overwrite existing config files with the selected source
        #[arg(long)]
        force: bool,
        /// Prefix of new issue ids (default: from the directory name)
        #[arg(long)]
        prefix: Option<String>,
        /// Write only project identity and base; use shared user configuration
        #[arg(long, conflicts_with_all = ["preset", "from", "list_presets"])]
        minimal: bool,
        /// Install shared agents and types with a landing preset (default: local)
        #[arg(long, value_name = "NAME", conflicts_with = "from")]
        preset: Option<String>,
        /// Install files from this directory instead of the built-in seed
        #[arg(long, value_name = "DIR")]
        from: Option<PathBuf>,
        /// List built-in presets without installing anything
        #[arg(long, conflicts_with_all = ["force", "prefix", "preset", "from"])]
        list_presets: bool,
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
        /// Issue type (default: [issues] default_type in config.toml)
        #[arg(long = "type")]
        issue_type: Option<String>,
        /// An issue that must close before this one is ready (repeatable)
        #[arg(long = "blocked-by", value_name = "ID")]
        blocked_by: Vec<String>,
        /// An issue that cannot be ready until this one closes (repeatable)
        #[arg(long, value_name = "ID")]
        blocks: Vec<String>,
        /// Use this id instead of allocating one (imports)
        #[arg(long, hide = true)]
        id: Option<String>,
        /// Immediately start an agent on the new issue, as with `trk run <ID>`
        #[arg(long, conflicts_with = "dry_run")]
        run: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Change title, description, priority or type; $EDITOR with no options
    Edit {
        id: String,
        #[arg(short = 't', long)]
        title: Option<String>,
        /// Description: text, @file, or @- for stdin
        #[arg(short = 'd', long)]
        description: Option<String>,
        #[arg(short = 'p', long)]
        priority: Option<String>,
        /// Issue type
        #[arg(long = "type")]
        issue_type: Option<String>,
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
        any other exit or timeout wakes the original live session, otherwise reopens the issue;\n\
        the output is kept in a note. Failed wake delivery also reopens the issue.\n\
        [check] max_wakes caps wakes per session (default 5; 0 disables waking).\n\
        At the cap, failures reopen with an output note. A fresh claim resets the count.\n\
        Without --check the issue waits for a human to close, reopen, or explicitly resume it.\n\
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
    /// Human-authorized continuation of the original live waiting session
    #[command(
        after_help = "Requires a human actor and a nonempty continuation message. Restores the original\n\
        live session's claim with a fresh lease before sending instructions; never starts a replacement.\n\
        Works with or without a waiting check and does not consume or reset automatic check wakes.\n\
        If the session is gone, a human must reopen the issue and use `trk run`.\n\
        `trk send` only delivers text; it never restores tracker ownership."
    )]
    Resume {
        id: String,
        /// Human authorization and instructions for the next work cycle (required)
        #[arg(short = 'm', long, num_args = 0..=1, default_missing_value = "")]
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
        /// Show what would run and be cleaned up, without changing anything
        #[arg(long)]
        dry_run: bool,
        /// Skip automatic closed-issue cleanup for this run or watch loop
        #[arg(long)]
        no_gc: bool,
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
    /// Attach a web address to an issue (a pull request, a preview); the dashboard opens it
    Link {
        id: String,
        url: String,
        /// Short display text, e.g. "PR #12"; again with the same URL to change it
        #[arg(short, long)]
        label: Option<String>,
    },
    /// Remove a link from an issue
    Unlink { id: String, url: String },
    /// Timeline: events and notes
    Log { id: String },
    /// Start an agent session; defaults to the first ready issue
    Run {
        id: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        harness: Option<String>,
        /// Override the agent or harness profile model for this run
        #[arg(long)]
        model: Option<String>,
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
    /// Active and waiting agent sessions with state, terminal and lease
    Runs,
    /// Live overview: what needs you, sessions, waiting checks, ready and blocked work
    #[command(
        visible_alias = "dash",
        after_help = "Opens a full-screen view that follows the database. Press ? for the keys.\n\
        Without a terminal, or with --json, it prints the same snapshot once."
    )]
    Dashboard,
    /// Attach to the session's terminal
    Attach { id: String },
    /// Type text into the session's terminal
    Send { id: String, text: String },
    /// Release claims whose session has ended, with a note
    Reap,
    /// Remove safe leftovers of closed issues (done by default)
    Gc {
        /// Issues to inspect (default: all closed issues)
        ids: Vec<String>,
        /// Report what would be removed without changing anything
        #[arg(long)]
        dry_run: bool,
        /// Also clean up issues closed as abandoned
        #[arg(long)]
        abandoned: bool,
        /// Keep issues closed more recently than this duration, e.g. 1d
        #[arg(long, value_name = "DURATION")]
        older_than: Option<String>,
    },
    /// The per-issue MCP server (started by the harness)
    Mcp {
        #[arg(long)]
        issue: String,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        agent: String,
    },
    /// List agent definitions and their effective models
    Agents,
    /// Inspect agent definitions
    Agent {
        #[command(subcommand)]
        cmd: AgentCmd,
    },
    /// The dashboard as a panel in the Herdr app
    #[command(
        after_help = "The mod is installed once per machine; a Herdr space shows it once a pane or tab\n\
        there holds it. `trk herdr open` does both: it installs or refreshes the mod when needed,\n\
        then opens the panel for this project in the space this terminal is in. The panel stays\n\
        in that space until you close it in Herdr."
    )]
    Herdr {
        #[command(subcommand)]
        cmd: HerdrCmd,
    },
    /// Resolved actor, project directory and database
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
enum HerdrCmd {
    /// Install or refresh the mod in ~/.config/herdr-app (once per machine)
    Install {
        /// Also write layouts: main-trk (sidebar dashboard button) and trk (split view)
        #[arg(long)]
        layout: bool,
        /// Overwrite a layout you have edited
        #[arg(long)]
        force: bool,
        /// Only refresh a mod that is installed already (what install.sh runs)
        #[arg(long)]
        if_installed: bool,
    },
    /// Show the panel for this project in a Herdr space; again: goes to the one that is there
    Open {
        /// Beside this terminal's pane instead of as a tab
        #[arg(long, value_parser = ["right", "down"])]
        split: Option<String>,
        /// The space, by id from `herdr-app ls` (default: this terminal's, else the project's)
        #[arg(long)]
        space: Option<String>,
    },
    /// Remove the mod and its layouts unless you edited them
    Uninstall {
        /// Remove an edited layout too
        #[arg(long)]
        force: bool,
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

fn main() {
    // Restore the exec handoff before any threads or environment readers start.
    let completed = check::resume().unwrap_or_else(|e| {
        eprintln!("trk: cannot resume check after restart: {e}");
        std::process::exit(1);
    });
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
                if let Some(hint) = e.downcast_ref::<Error>().and_then(Error::hint) {
                    eprintln!("hint: {hint}");
                }
                1
            }
        };
        std::process::exit(code);
    }
    match dispatch(&g, cli.cmd, completed, &r) {
        Ok(out) => r.emit(out),
        Err(e) => {
            r.error(&e);
            std::process::exit(e.exit_code());
        }
    }
}

fn mcp(g: &Global, issue: &str, actor: &str, agent: &str) -> anyhow::Result<()> {
    let env = Env::new(g)?;
    trk_mcp::serve(trk_mcp::McpArgs {
        db: env.db.clone(),
        config: env.config.clone(),
        opts: env.store_opts()?,
        base: env.base(),
        project_root: env.root,
        user: env.user,
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

fn dispatch(g: &Global, cmd: Cmd, completed: Vec<Value>, r: &Renderer) -> Result<Out> {
    if let Cmd::Install {
        force,
        prefix,
        minimal,
        preset,
        from,
        list_presets,
    } = cmd
    {
        if list_presets {
            return Ok(Out::Text(
                seed::PRESETS.join("\n"),
                json!({"presets": seed::PRESETS}),
            ));
        }
        return install(
            g,
            force,
            prefix,
            preset.as_deref().unwrap_or("local"),
            from.as_deref(),
            minimal,
        );
    }
    // The mod is machine-wide: installing and removing it need no project.
    let cmd = match cmd {
        Cmd::Herdr {
            cmd:
                HerdrCmd::Install {
                    layout,
                    force,
                    if_installed,
                },
        } => {
            return herdr_ui::install(
                &herdr_ui::app_home(),
                herdr_ui::InstallOpts {
                    layout,
                    force,
                    if_installed,
                },
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
        Cmd::Herdr { cmd } => {
            let HerdrCmd::Open { split, space } = cmd else {
                unreachable!()
            };
            let place = match split.as_deref() {
                Some("right") => herdr_ui::Place::Right,
                Some("down") => herdr_ui::Place::Down,
                _ => herdr_ui::Place::Tab,
            };
            herdr_ui::open(&env, &herdr_ui::app_home(), place, space.as_deref())
        }
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
                let mut issue = store.read(env.actor.as_deref(), |c| c.show(id, w))?;
                if store
                    .opts
                    .known_types
                    .iter()
                    .any(|name| Some(name.as_str()) == issue["type"].as_str())
                {
                    let cfg = trk_agent::routing::RoutingConfig::new(&env.config);
                    issue["routing"] =
                        json!(cfg.resolve(issue["type"].as_str().unwrap_or_default(), None)?);
                }
                items.push(issue);
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
            issue_type,
            blocked_by,
            blocks,
            id,
            run,
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
                issue_type,
                blocked_by,
                blocks,
                ..Default::default()
            },
            dry_run,
            run,
        ),
        Cmd::Edit {
            id,
            title,
            description,
            priority,
            issue_type,
            dry_run,
        } => edit(
            &env,
            id,
            Edit {
                title,
                description: description.as_deref().map(read_value).transpose()?,
                priority,
                issue_type,
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
                    // working directory when it is a checkout of this project.
                    let worktree =
                        explicit.or_else(|| env.here().map(|p| p.to_string_lossy().to_string()));
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
        Cmd::Link { id, url, label } => {
            let actor = env.actor()?.to_string();
            with_open(&env, |s| {
                let (id, changed, links) = s.write(&actor, false, |c| {
                    let id = c.resolve_id(&id)?;
                    let changed = c.link_add(&id, &url, label.as_deref())?;
                    Ok((id.clone(), changed, c.links(&id)?))
                })?;
                Ok(Out::Text(
                    format!(
                        "linked {id} to {}{}",
                        url.trim(),
                        if changed { "" } else { " (already present)" }
                    ),
                    json!({"id": id, "changed": changed, "links": links}),
                ))
            })
        }
        Cmd::Unlink { id, url } => {
            let actor = env.actor()?.to_string();
            with_open(&env, |s| {
                let (id, links) = s.write(&actor, false, |c| {
                    let id = c.resolve_id(&id)?;
                    c.link_rm(&id, &url)?;
                    Ok((id.clone(), c.links(&id)?))
                })?;
                Ok(Out::Text(
                    format!("unlinked {id} from {}", url.trim()),
                    json!({"id": id, "links": links}),
                ))
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
        Cmd::Gc {
            ids,
            dry_run,
            abandoned,
            older_than,
        } => gc::run(&env, ids, dry_run, abandoned, older_than.as_deref()),
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
        Cmd::Agent { cmd } => agent_cmd(&env, cmd),
        Cmd::Agents => agent_cmd(&env, AgentCmd::List),
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

/// The default issue id prefix for a project directory: its name, reduced to
/// lowercase letters and digits.
fn default_prefix(root: &std::path::Path) -> String {
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let slug: String = name
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect();
    let slug = slug.trim_start_matches(|c: char| c.is_ascii_digit());
    if slug.is_empty() {
        "trk".into()
    } else {
        slug.to_string()
    }
}

type InstallFiles = BTreeMap<PathBuf, Cow<'static, [u8]>>;

fn read_install_files(root: &Path, dir: &Path, files: &mut InstallFiles) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| bad_args(format!("cannot read install source {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            read_install_files(root, &path, files)?;
        } else if kind.is_file() {
            files.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                Cow::Owned(std::fs::read(&path)?),
            );
        } else {
            return Err(bad_args(format!(
                "install source entries must be regular files or directories: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct DetectedProfiles {
    launchers: Vec<String>,
    harnesses: Vec<String>,
}

/// Filter the built-in seed without reserializing its comments and templates.
fn detect_seed_profiles(config: &mut String) -> DetectedProfiles {
    let source: toml::Table = toml::from_str(config).expect("rendered seed is valid TOML");
    let mut detected = DetectedProfiles {
        launchers: Vec::new(),
        harnesses: Vec::new(),
    };
    for (section, key, found) in [
        ("launcher", "use", &mut detected.launchers),
        ("harness", "prefer", &mut detected.harnesses),
    ] {
        let candidates = &source[section][key];
        for name in candidates.as_array().expect("seed preference is an array") {
            let name = name.as_str().expect("seed profile name is a string");
            let profile = &source[section][name];
            let available = if section == "launcher" {
                let profile: launcher::Launcher =
                    profile.clone().try_into().expect("valid seeded launcher");
                profile.is_available()
            } else {
                trk_agent::harness::installed(
                    profile["command"].as_str().expect("seed harness command"),
                )
            };
            if available {
                found.push(name.to_owned());
            }
        }
        let value = toml::Value::Array(found.iter().cloned().map(toml::Value::String).collect());
        *config = config.replace(
            &format!("{key} = {candidates}"),
            &format!("{key} = {value}"),
        );
    }
    detected
}

/// Install the selected source and database, and refresh the agent skill and
/// installed Herdr mod. Existing config files are kept unless `force`.
fn install(
    g: &Global,
    force: bool,
    prefix: Option<String>,
    preset: &str,
    from: Option<&Path>,
    minimal: bool,
) -> Result<Out> {
    let mut files: InstallFiles = if minimal {
        BTreeMap::from([
            (PathBuf::from(".gitignore"), Cow::Borrowed(seed::GITIGNORE)),
            (
                PathBuf::from("config.toml"),
                Cow::Borrowed(
                    b"[user]\n{{EMAIL_LINE}}\n\n[issues]\nprefix = \"{{PREFIX}}\"\n\n[run]\nbase = {{BASE}}\n"
                        .as_slice(),
                ),
            ),
        ])
    } else if let Some(root) = from {
        let mut files = BTreeMap::new();
        read_install_files(root, root, &mut files)?;
        files
    } else {
        seed::preset(preset)
            .ok_or_else(|| {
                bad_args(format!("unknown install preset {preset:?}"))
                    .with_hint("run `trk install --list-presets` to see available presets")
            })?
            .into_iter()
            .map(|(path, content)| (PathBuf::from(path), Cow::Borrowed(content)))
            .collect()
    };
    let landing = if minimal || from.is_some() {
        "merge"
    } else {
        seed::landing(preset).expect("built-in preset has a landing recipe")
    };
    let loc = env::Location::find(g);
    let dir = loc.dir.clone();
    let root = loc.root.clone();
    let config_path = dir.join("config.toml");
    if let Some(p) = &prefix {
        trk_core::store::check_prefix(p)?;
        if config_path.exists() && !force {
            return Err(bad_args(format!(
                "{} already exists, so --prefix has no effect",
                config_path.display()
            ))
            .with_hint("edit [issues] prefix in it, or pass --force to rewrite the config"));
        }
    }
    std::fs::create_dir_all(&dir)?;
    let email = g
        .actor
        .as_deref()
        .and_then(|a| a.strip_prefix("human:"))
        .map(String::from)
        .or_else(|| env::git(&root, &["config", "user.email"]).filter(|e| !e.is_empty()));
    let prefix = prefix.unwrap_or_else(|| default_prefix(&root));
    let mut detected = None;
    if let Some(content) = files.get_mut(Path::new("config.toml")) {
        let template = std::str::from_utf8(content)
            .map_err(|e| bad_args(format!("install source config.toml is not UTF-8: {e}")))?;
        let mut rendered = template
            .replace(
                "{{EMAIL_LINE}}",
                &match &email {
                    Some(e) => format!("email = {}", toml::Value::String(e.clone())),
                    None => "# email = \"you@example.com\"".to_string(),
                },
            )
            .replace("{{PREFIX}}", &prefix)
            .replace("{{DEFAULT_TYPE}}", seed::DEFAULT_TYPE)
            .replace(
                "{{BASE}}",
                &toml::Value::String(env::detect_base(&root)).to_string(),
            )
            .replace(
                "{{LANDING}}",
                &toml::Value::String(landing.to_owned()).to_string(),
            );
        if !minimal && from.is_none() && (!config_path.exists() || force) {
            detected = Some(detect_seed_profiles(&mut rendered));
        }
        *content = Cow::Owned(rendered.into_bytes());
    }
    let mut written = vec![];
    let mut kept = vec![];
    // Compare kept files with the selected source, not another preset.
    let mut differ = vec![];
    for (rel, content) in files {
        let p = dir.join(&rel);
        let name = rel.to_string_lossy().into_owned();
        if p.exists() && !force {
            kept.push(name.clone());
            if std::fs::read(&p).is_ok_and(|installed| {
                if rel == Path::new("config.toml") {
                    // Compare profiles, not project identity or formatting.
                    let parse = |bytes: &[u8]| {
                        std::str::from_utf8(bytes)
                            .ok()
                            .and_then(|s| toml::from_str::<toml::Table>(s).ok())
                    };
                    match (parse(&installed), parse(&content)) {
                        (Some(mut installed), Some(mut source)) => {
                            if from.is_none() {
                                // Install-time preferences are machine-specific, not profile drift.
                                for config in [&mut installed, &mut source] {
                                    if let Some(harness) = config
                                        .get_mut("harness")
                                        .and_then(toml::Value::as_table_mut)
                                    {
                                        harness.remove("prefer");
                                    }
                                }
                            }
                            installed.get("harness") != source.get("harness")
                        }
                        _ => true,
                    }
                } else {
                    installed != content.as_ref()
                }
            }) {
                differ.push(name);
            }
            continue;
        }
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, content)?;
        written.push(name);
    }
    let env = Env::at(loc, g)?;
    let created = !env.db.is_file();
    trk_core::Store::install(&env.db, env.store_opts()?)?;
    let skill = skill::install(&root, env.checkout().is_some(), &env.user.skill)?;
    let herdr = herdr_ui::refresh(&herdr_ui::app_home())?;

    let mut text = format!(
        "project   {}\nconfig    {}\n          wrote {} files{}\ndatabase  {}{}\nprefix    {}\nskill     {}",
        root.display(),
        dir.display(),
        written.len(),
        if kept.is_empty() {
            String::new()
        } else {
            format!(", kept {} existing (use --force to overwrite)", kept.len())
        },
        env.db.display(),
        if created { " (created)" } else { "" },
        env.user.issues.prefix,
        skill.lines.join("\n          "),
    );
    if let Some((line, _)) = &herdr {
        text.push_str(&format!("\nherdr     {line}"));
    }
    if let Some(detected) = &detected {
        for (label, names) in [
            ("launchers", &detected.launchers),
            ("harnesses", &detected.harnesses),
        ] {
            text.push_str(&format!(
                "\n{label:10}{}",
                if names.is_empty() {
                    "none detected".to_owned()
                } else {
                    names.join(" > ")
                }
            ));
        }
    }
    if !differ.is_empty() {
        text.push_str(&format!(
            "\n\nKept files that differ from the selected install source (your edits, or newer defaults):\n  {}",
            differ.join("\n  ")
        ));
    }
    let mut warnings = skill.warnings.clone();
    if !minimal && from.is_none() {
        match env.user.run.landing.as_str() {
            "merge" if env.user.check.verify.trim().is_empty() => warnings.push(
                "local landing has no review or approval gate and [check] verify is empty: merges will skip verification; configure it before running agents".to_owned(),
            ),
            "pr" => {
                warnings.push(
                    "GitHub landing requires authenticated `gh`; run `gh auth login` before running agents".to_owned(),
                );
                if !trk_agent::harness::installed("gh") {
                    warnings.push(
                        "`gh` is not on PATH: install the GitHub CLI for pull-request landing".to_owned(),
                    );
                }
            }
            _ => {}
        }
    }
    if let Some(detected) = &detected {
        if detected.launchers.is_empty() {
            warnings.push(
                "no launcher detected: install Herdr, hc, or tmux and edit [launcher] use in config.toml, or run `trk run --fg`".to_owned(),
            );
        }
        if detected.harnesses.is_empty() {
            warnings.push(
                "no harness detected: install omp, claude, codex, opencode, or pi and edit [harness] prefer in config.toml".to_owned(),
            );
        }
    }
    if env.actor.is_none() {
        warnings.push(format!(
            "no email found: set [user] email in {}",
            config_path.display()
        ));
    }
    for w in &warnings {
        text.push_str(&format!("\nwarning: {w}"));
    }
    text.push_str(
        "\n\nNext:\n  trk create \"…\" -d \"…\"\n  trk run                          (start an agent on the first ready issue)\n  trk check --watch                (in a spare terminal: runs waiting checks)",
    );
    Ok(Out::Text(
        text,
        json!({"root": root, "dir": dir, "db": env.db, "db_created": created,
               "prefix": env.user.issues.prefix, "written": written, "kept": kept,
               "differ": differ, "detected": detected, "skill": skill.data,
               "herdr": herdr.map(|(_, data)| data), "warnings": warnings}),
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
        c.list(q.trim(), sort.as_deref(), limit)
    })?;
    Ok(Out::Issues { items, fields })
}

fn create(env: &Env, new: NewIssue, dry_run: bool, run: bool) -> Result<Out> {
    let actor = env.actor()?.to_string();
    let mut store = env.store()?;
    let mut attempt = |new: NewIssue| -> Result<Value> {
        store.write(&actor, dry_run, |c| {
            let id = c.create(new)?;
            c.show(&id, SHOWN)
        })
    };
    let v = if new.title.trim().is_empty() && editor::is_tty() && !dry_run {
        let doc = editor::Doc {
            title: String::new(),
            priority: new
                .priority
                .clone()
                .unwrap_or_else(|| DEFAULT_PRIORITY.to_string()),
            issue_type: new
                .issue_type
                .clone()
                .unwrap_or_else(|| env.user.issues.default_type.clone()),
            description: if new.description.is_empty() {
                "<!-- What is needed and why, with enough context to start cold. -->\n".into()
            } else {
                new.description.clone()
            },
        };
        editor::edit_loop(&doc, |d| {
            let description = d.description.trim();
            let placeholder = description.starts_with("<!--") && description.ends_with("-->");
            attempt(NewIssue {
                title: d.title,
                priority: Some(d.priority).filter(|p| !p.trim().is_empty()),
                issue_type: Some(d.issue_type),
                description: if placeholder {
                    String::new()
                } else {
                    d.description
                },
                ..new.clone()
            })
        })?
    } else {
        attempt(new)?
    };
    if run {
        let id = v["id"].as_str().unwrap_or_default();
        return run::run(
            env,
            run::RunArgs {
                id: Some(id.to_string()),
                agent: None,
                harness: None,
                model: None,
                fg: false,
                print: false,
                dry_run: false,
            },
        )
        .map_err(|err| {
            let err = match err {
                Error::Coded(_) => err,
                other => Error::new(other.code(), other.to_string()),
            };
            err.with_issue(id)
        });
    }
    Ok(Out::Issue(v))
}

fn edit(env: &Env, id: String, e: Edit, dry_run: bool) -> Result<Out> {
    let actor = env.actor()?.to_string();
    let mut store = env.store()?;
    let nothing = e.title.is_none()
        && e.description.is_none()
        && e.priority.is_none()
        && e.issue_type.is_none();
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
            .with_hint("pass -t TITLE, -d DESCRIPTION, -p PRIORITY or --type NAME"));
    }
    let cur = store.read(None, |c| {
        let id = c.resolve_id(&id)?;
        c.issue(&id)
    })?;
    let doc = editor::Doc {
        title: cur.title.clone(),
        priority: cur.priority.clone(),
        issue_type: cur.issue_type.clone(),
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
                    issue_type: (d.issue_type.trim() != cur.issue_type).then_some(d.issue_type),
                },
            )?;
            c.show(&cur.id, SHOWN)
        })
    })?;
    Ok(Out::Issue(v))
}

fn whoami(env: &Env) -> Result<Out> {
    let store = env.store()?;
    let (counts, lock) = store.read(None, |c| Ok((c.counts()?, c.check_lock()?)))?;
    let data = json!({
        "version": env!("TRK_VERSION"),
        "actor": env.actor,
        "root": env.root,
        "dir": env.dir,
        "user_dir": env.config.user_path,
        "db": env.db,
        "prefix": env.user.issues.prefix,
        "worktrees": env.worktrees_dir(),
        "skill": skill::configured_state(&env.root, &env.user.skill),
        "default_type": env.user.issues.default_type,
        "issues": counts,
        "check_running": lock.map(|(pid, host, started, pgid)| json!({"pid": pid, "host": host, "started_at": started, "pgid": pgid})),
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
    let local_config = || trk_agent::routing::RoutingConfig::new(&env.config);
    match cmd {
        AgentCmd::List => {
            let cfg = local_config();
            let agents = cfg.agents()?;
            let types = cfg.types()?;
            let profiles = trk_agent::harness::profiles(&env.user)?;
            let mut items = vec![];
            let mut text = String::new();
            for resolved in agents.values() {
                let a = &resolved.agent;
                let source = resolved.config.resolve(format!("agents/{}.toml", a.name));
                let source_layer = resolved.config.source_layer(&source);
                let templates =
                    trk_agent::prompt::agent_template_sources(&resolved.config, a, &env.user)?;
                let agent_types: Vec<_> = types.values().filter(|r| r.agent == a.name).collect();
                let models: BTreeMap<_, _> = profiles
                    .iter()
                    .map(|(name, profile)| (name, profile.resolve_model(a, name, None)))
                    .collect();
                let model_text = models
                    .iter()
                    .map(|(name, model)| format!("{name}={}", model.unwrap_or("(harness default)")))
                    .collect::<Vec<_>>()
                    .join(", ");
                let tools: Vec<String> = trk_agent::agent_tools(a, &env.user)
                    .into_iter()
                    .map(|t| t.name)
                    .collect();
                text.push_str(&format!(
                    "{:<12} {}\n             harness: {}  builtin_tools: {}\n             model: {}\n             tools: {}\n             source: {} ({})\n",
                    a.name,
                    a.description.clone().unwrap_or_default(),
                    a.harness.prefer.as_deref().unwrap_or_default().join(" > "),
                    a.harness.builtin_tools,
                    model_text,
                    tools.join(", "),
                    source.display(),
                    source_layer,
                ));
                for route in &agent_types {
                    text.push_str(&format!(
                        "             type {}: {} ({})\n",
                        route.issue_type,
                        route.type_source.display(),
                        route.type_layer,
                    ));
                }
                for (name, template) in &templates {
                    text.push_str(&format!(
                        "             template {name}: {} ({})\n",
                        template.source.display(),
                        template.layer,
                    ));
                }
                items.push(json!({"name": a.name, "description": a.description,
                                  "tools": tools,
                                  "model": a.harness.model, "models": models, "args": a.harness.args,
                                  "source": source, "source_layer": source_layer,
                                  "prompt_directory": resolved.config.path, "templates": templates,
                                  "types": agent_types,
                                  "harness": a.harness.prefer, "builtin_tools": a.harness.builtin_tools}));
            }
            if text.is_empty() {
                text = "no agents: run `trk install`".into();
            }
            Ok(Out::Text(text, Value::Array(items)))
        }
        AgentCmd::Check => {
            let cfg = local_config();
            let d = cfg.check(&env.user);
            let errors = d.iter().filter(|x| x.level == Level::Error).count();
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
            let mut resources = json!({"agents": [], "types": [], "templates": {}});
            let mut resource_text = String::new();
            let mut templates = BTreeMap::new();
            for resolved in cfg.agents()?.values() {
                let source = resolved
                    .config
                    .resolve(format!("agents/{}.toml", resolved.agent.name));
                let layer = resolved.config.source_layer(&source);
                resources["agents"].as_array_mut().unwrap().push(json!({
                    "name": resolved.agent.name, "source": source, "source_layer": layer,
                }));
                resource_text.push_str(&format!(
                    "\nagent {}: {} ({layer})",
                    resolved.agent.name,
                    source.display(),
                ));
                templates.extend(trk_agent::prompt::agent_template_sources(
                    &resolved.config,
                    &resolved.agent,
                    &env.user,
                )?);
            }
            for route in cfg.types()?.values() {
                let resolved = cfg.load(route)?;
                let prompts = trk_agent::prompt::type_prompts(
                    &resolved.config,
                    &resolved.agent,
                    &env.user,
                    route,
                )?;
                templates.extend(prompts.sources);
                resources["types"].as_array_mut().unwrap().push(json!({
                    "name": route.issue_type, "source": route.type_source,
                    "source_layer": route.type_layer,
                    "agent": route.agent, "guidance": prompts.guidance,
                }));
                resource_text.push_str(&format!(
                    "\ntype {}: {} ({})",
                    route.issue_type,
                    route.type_source.display(),
                    route.type_layer,
                ));
                if let Some(guidance) = &prompts.guidance {
                    resource_text.push_str(&format!("\n  guidance:\n{guidance}"));
                }
            }
            for (name, template) in &templates {
                resource_text.push_str(&format!(
                    "\ntemplate {name}: {} ({})",
                    template.source.display(),
                    template.layer,
                ));
            }
            resources["templates"] = serde_json::to_value(templates)?;
            let data = json!({"ok": errors == 0, "diagnostics": d, "resources": resources});
            let mut text = if d.is_empty() {
                "agents, templates and harness profiles are fine".to_string()
            } else {
                diag_text(&d)
            };
            text.push_str(&resource_text);
            Ok(Out::Text(text, data))
        }
        AgentCmd::Preview { agent, issue } => {
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
                None => trk_agent::check::sample_issue(&env.user),
            };
            let cfg = trk_agent::routing::RoutingConfig::new(&env.config);
            let routing = cfg.resolve(
                issue_json["type"].as_str().unwrap_or_default(),
                Some(&agent),
            )?;
            let resolved = cfg.load(&routing)?;
            let a = resolved.agent;
            let tools = trk_agent::agent_tools(&a, &env.user);
            let input = trk_agent::prompt::PromptInput {
                issue: &issue_json,
                project_root: &env.root,
                config: &env.user,
                issue_type: &routing.issue_type,
                type_description: &routing.description,
                type_guidance: routing.guidance.as_deref(),
                agent: &a,
                tools: &tools,
                worktree: issue_json["claim"]["worktree"].as_str(),
                actor: "agent:<harness>:<session>",
                harness: None,
                base: &env.base(),
                verify: &env.user.check.verify,
            };
            let prompts = trk_agent::prompt::render_prompts(&resolved.config, &input, false)?;
            let instructions = prompts.instructions;
            let briefing = prompts.briefing;
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
                "== routing ==\ntype: {} · agent: {}\ntype source: {} ({})\nagent source: {} ({})\nprompts: {}\n\n== instructions (MCP server) ==\n{instructions}\n\n== briefing (initial prompt) ==\n{briefing}\n",
                routing.issue_type,
                routing.agent,
                routing.type_source.display(),
                routing.type_layer,
                routing.agent_source.display(),
                routing.agent_layer,
                routing.prompt_directory.display(),
            );
            for (name, template) in &prompts.sources {
                text.push_str(&format!(
                    "\ntemplate {name}: {} ({})\n",
                    template.source.display(),
                    template.layer,
                ));
            }
            text.push_str("\n== trk tools ==\n");
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
                json!({"agent": a.name, "type": routing.issue_type, "routing": routing, "instructions": instructions, "briefing": briefing, "guidance": prompts.guidance, "templates": prompts.sources, "tools": tool_json, "mcp": others}),
            ))
        }
    }
}
