//! `trk`: a local issue tracker and agent launcher.

mod editor;
mod env;
mod render;
mod run;
mod seed;
mod skill;

use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use trk_core::config::{Level, check_schema};
use trk_core::error::{Code, Error, Result, bad_args};
use trk_core::fields::{Assign, parse_assign};
use trk_core::ops::{ClaimOpts, NewDep, NewIssue};
use trk_core::registry::Category;
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

#[derive(Args, Clone, Default)]
struct FieldArgs {
    /// Set a field: k=v, k+=item, k-=item, k=@file, k=@-, k= (clear)
    #[arg(short = 'f', long = "field", value_name = "K=V")]
    fields: Vec<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write the default config and apply it
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
    /// Shorthand for `trk list 'ready …'`
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
        #[arg(long, default_value = "deps")]
        with: String,
    },
    /// Create an issue; opens $EDITOR when required fields are missing on a TTY
    Create {
        #[arg(value_name = "TYPE")]
        ty: String,
        #[arg(short = 't', long)]
        title: Option<String>,
        #[command(flatten)]
        f: FieldArgs,
        /// Whole object, same shape as `show --json` (- for stdin)
        #[arg(long)]
        from: Option<String>,
        /// Add an edge from the new issue: KIND:ID (e.g. blocks:orch-m1)
        #[arg(long = "dep", value_name = "KIND:ID")]
        deps: Vec<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Change field values; $EDITOR with no arguments
    Edit {
        id: String,
        #[command(flatten)]
        f: FieldArgs,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Claim and enter the first intermediate status
    Claim {
        id: String,
        /// Lease, e.g. 30m (agent claims default to the configured lease)
        #[arg(long)]
        lease: Option<String>,
        #[arg(long)]
        worktree: Option<PathBuf>,
        #[arg(long)]
        steal: bool,
        #[command(flatten)]
        f: FieldArgs,
    },
    /// Release the claim and return to open
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
    /// Move between intermediate statuses
    Move {
        id: String,
        status: String,
        #[command(flatten)]
        f: FieldArgs,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Close; checks required fields and gates
    Close {
        id: String,
        #[arg(short = 'r', long)]
        resolution: String,
        #[command(flatten)]
        f: FieldArgs,
        #[arg(long)]
        force: bool,
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
    /// Add or remove an edge: SRC KIND DST
    Dep {
        #[command(subcommand)]
        cmd: DepCmd,
    },
    /// Merged timeline: events, notes, claims
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
        #[command(flatten)]
        f: FieldArgs,
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
    /// Validate, diff, apply and draw the configuration; with no subcommand, show the applied schema
    #[command(args_conflicts_with_subcommands = true)]
    Schema {
        #[command(subcommand)]
        cmd: Option<SchemaCmd>,
        #[arg(value_name = "TYPE")]
        ty: Option<String>,
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
    Add {
        src: String,
        kind: String,
        dst: String,
    },
    Rm {
        src: String,
        kind: String,
        dst: String,
    },
}

#[derive(Subcommand)]
enum AgentCmd {
    List,
    /// What a session would receive: instructions, briefing, tools, MCP servers
    Preview {
        agent: String,
        #[arg(long = "type", conflicts_with = "issue")]
        ty: Option<String>,
        #[arg(long)]
        issue: Option<String>,
    },
}

#[derive(Subcommand)]
enum SchemaCmd {
    /// Check the config files
    Check,
    /// Diff the config files against the database
    Plan {
        #[arg(long = "map-status", value_name = "OLD=NEW")]
        map_status: Vec<String>,
    },
    /// Apply the config files in one transaction
    Apply {
        #[arg(long = "map-status", value_name = "OLD=NEW")]
        map_status: Vec<String>,
    },
    /// Draw a type's state machine (Mermaid, or DOT with --dot)
    Graph {
        #[arg(value_name = "TYPE")]
        ty: String,
        #[arg(long)]
        dot: bool,
    },
    /// Add an expression index for a hot custom field
    Index { field: String },
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
        },
        issue: issue.to_string(),
        actor: actor.to_string(),
        agent: agent.to_string(),
    })
}

/// Read `@file` / `@-` values.
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

fn parse_fields(f: &FieldArgs) -> Result<Vec<Assign>> {
    let mut out = vec![];
    for s in &f.fields {
        let (field, op, raw) = parse_assign(s)?;
        out.push(Assign {
            field,
            op,
            value: Value::String(read_value(&raw)?),
        });
    }
    Ok(out)
}

const TOOL_OWNED: &[&str] = &[
    "id",
    "type",
    "project",
    "status",
    "status_category",
    "resolution",
    "claim",
    "deps",
    "derived",
    "created_by",
    "created_at",
    "updated_at",
    "closed_at",
    "notes",
    "events",
    "claims",
];

/// `--from FILE|-`: the shape `show --json` emits (bare or in the envelope).
fn from_file(path: &str) -> Result<Vec<Assign>> {
    let text = if path == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else {
        std::fs::read_to_string(path).map_err(|e| bad_args(format!("cannot read {path}: {e}")))?
    };
    let mut v: Value =
        serde_json::from_str(&text).map_err(|e| bad_args(format!("{path}: invalid JSON: {e}")))?;
    if v.get("data").is_some() && v.get("ok").is_some() {
        v = v["data"].take();
    }
    let obj = v
        .as_object()
        .ok_or_else(|| bad_args("--from expects a JSON object"))?;
    let mut out = vec![];
    let mut ignored = vec![];
    for (k, val) in obj {
        if TOOL_OWNED.contains(&k.as_str()) {
            ignored.push(k.clone());
            continue;
        }
        if k == "fields" {
            for (fk, fv) in val.as_object().into_iter().flatten() {
                if !fv.is_null() {
                    out.push(Assign::set(fk.clone(), fv.clone()));
                }
            }
            continue;
        }
        if !val.is_null() {
            out.push(Assign::set(k.clone(), val.clone()));
        }
    }
    if !ignored.is_empty() {
        eprintln!(
            "warning: --from ignores tool-owned fields: {}",
            ignored.join(", ")
        );
    }
    Ok(out)
}

fn with_open<T>(env: &Env, f: impl FnOnce(&mut trk_core::Store) -> Result<T>) -> Result<T> {
    let mut s = env.store()?;
    f(&mut s)
}

fn show_one(env: &Env, store: &trk_core::Store, id: &str) -> Result<Out> {
    let actor = env.actor.clone();
    Ok(Out::Issue(store.read(actor.as_deref(), |c| {
        c.show(
            id,
            With {
                deps: true,
                ..Default::default()
            },
        )
    })?))
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
            ty,
            title,
            f,
            from,
            deps,
            dry_run,
        } => create(&env, ty, title, f, from, deps, dry_run),
        Cmd::Edit {
            id,
            f,
            from,
            dry_run,
        } => edit(&env, id, f, from, dry_run),
        Cmd::Claim {
            id,
            lease,
            worktree,
            steal,
            f,
        } => {
            let actor = env.actor()?.to_string();
            let lease = lease
                .map(|l| trk_core::config::parse_duration(&l))
                .transpose()?;
            let mut opts = ClaimOpts {
                lease,
                steal,
                assigns: parse_fields(&f)?,
                host: env::hostname(),
                ..Default::default()
            };
            // Inside a git checkout, record where the work happens.
            let wt = match worktree {
                Some(p) => Some(
                    std::fs::canonicalize(&p)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .to_string(),
                ),
                None => env.git.as_ref().map(|g| g.toplevel.clone()),
            };
            if let Some(w) = &wt {
                opts.worktree = Some(w.clone());
                opts.branch = env::git_info(std::path::Path::new(w)).and_then(|g| g.branch);
            }
            with_open(&env, |s| {
                s.write(&actor, false, |c| {
                    let id = c.resolve_id(&id)?;
                    // Only keep the worktree when it is a checkout of the issue's repo.
                    let issue = c.issue(&id)?;
                    if let (Some(r), Some(g)) =
                        (issue.fields.get("repo").and_then(|v| v.as_str()), &env.git)
                    {
                        let matches = c.repo_by_ref(r)?.is_some_and(|repo| {
                            g.remote.as_deref().map(trk_core::model::normalize_remote)
                                == repo.remote_url
                                || repo.local_path.as_deref() == Some(g.toplevel.as_str())
                        });
                        if !matches && opts.worktree.as_deref() == Some(g.toplevel.as_str()) {
                            opts.worktree = None;
                            opts.branch = None;
                        }
                    }
                    c.claim(&id, opts).map(|_| ())
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
        Cmd::Move {
            id,
            status,
            f,
            force,
            dry_run,
        } => {
            let actor = env.actor()?.to_string();
            let assigns = parse_fields(&f)?;
            with_open(&env, |s| {
                s.write(&actor, dry_run, |c| {
                    c.move_to(&id, &status, &assigns, force)?;
                    c.show(
                        &id,
                        With {
                            deps: true,
                            ..Default::default()
                        },
                    )
                })
                .map(Out::Issue)
            })
        }
        Cmd::Close {
            id,
            resolution,
            f,
            force,
            dry_run,
        } => {
            let actor = env.actor()?.to_string();
            let assigns = parse_fields(&f)?;
            with_open(&env, |s| {
                s.write(&actor, dry_run, |c| {
                    c.close(&id, &resolution, &assigns, force)?;
                    c.show(
                        &id,
                        With {
                            deps: true,
                            ..Default::default()
                        },
                    )
                })
                .map(Out::Issue)
            })
        }
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
                DepCmd::Add { src, kind, dst } => {
                    let (added, src, dst) = s.write(&actor, false, |c| {
                        let (src, dst) = (c.resolve_id(&src)?, c.resolve_id(&dst)?);
                        Ok((c.dep_add(&src, &kind, &dst)?, src, dst))
                    })?;
                    Ok(Out::Text(
                        format!(
                            "{src} {kind} {dst}{}",
                            if added { "" } else { " (already present)" }
                        ),
                        json!({"src": src, "kind": kind, "dst": dst, "added": added}),
                    ))
                }
                DepCmd::Rm { src, kind, dst } => {
                    let (src, dst) = s.write(&actor, false, |c| {
                        let (src, dst) = (c.resolve_id(&src)?, c.resolve_id(&dst)?);
                        c.dep_rm(&src, &kind, &dst)?;
                        Ok((src, dst))
                    })?;
                    Ok(Out::Text(
                        format!("removed {src} {kind} {dst}"),
                        json!({"src": src, "kind": kind, "dst": dst}),
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
            f,
        } => run::run(
            &env,
            run::RunArgs {
                id,
                agent,
                harness,
                fg,
                print,
                dry_run,
                assigns: parse_fields(&f)?,
            },
        ),
        Cmd::Runs => run::runs(&env),
        Cmd::Attach { id } => run::attach(&env, &id),
        Cmd::Send { id, text } => run::send(&env, &id, &text),
        Cmd::Reap => run::reap(&env),
        Cmd::Mcp { .. } => unreachable!(),
        Cmd::Agent { cmd } => agent_cmd(&env, cmd),
        Cmd::Schema { cmd, ty } => schema_cmd(&env, cmd, ty),
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
    // Re-read the user config we may just have written, then apply.
    let env2 = Env {
        user: env.config.user_config()?,
        ..Env::new_from(env)
    };
    let applied = schema_apply(&env2, &[])?;
    let text = format!(
        "config: {}\n  wrote {} files{}\ndatabase: {}\n{}\n\nNext:\n  trk project add <id> --name \"…\"\n  trk repo add --project <id>      (inside a git checkout)\n  trk create code-change -t \"…\"",
        dir.display(),
        written.len(),
        if kept.is_empty() {
            String::new()
        } else {
            format!(", kept {} existing (use --force to overwrite)", kept.len())
        },
        env.db.display(),
        match &applied {
            Out::Text(t, _) => t.clone(),
            _ => String::new(),
        }
    );
    let data = match applied {
        Out::Text(_, d) => d,
        _ => Value::Null,
    };
    Ok(Out::Text(
        text,
        json!({"config": dir, "db": env.db, "written": written, "kept": kept, "applied": data}),
    ))
}

impl Env {
    fn new_from(e: &Env) -> Env {
        Env {
            db: e.db.clone(),
            config: e.config.clone(),
            user: e.user.clone(),
            actor: e.actor.clone(),
            explicit_project: e.explicit_project.clone(),
            git: e.git.clone(),
        }
    }
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

fn create(
    env: &Env,
    ty: String,
    title: Option<String>,
    f: FieldArgs,
    from: Option<String>,
    deps: Vec<String>,
    dry_run: bool,
) -> Result<Out> {
    let actor = env.actor()?.to_string();
    let mut store = env.store()?;
    let project = env.project(&store)?;
    let mut assigns = vec![];
    if let Some(p) = &from {
        assigns.extend(from_file(p)?);
    }
    if let Some(t) = title {
        assigns.push(Assign::set("title", t));
    }
    assigns.extend(parse_fields(&f)?);
    let mut new_deps = vec![];
    for d in deps {
        let (kind, other) = d
            .split_once(':')
            .ok_or_else(|| bad_args(format!("--dep {d}: expected KIND:ID")))?;
        new_deps.push(NewDep {
            kind: kind.into(),
            other: other.into(),
            new_is_src: true,
        });
    }
    let attempt = |store: &mut trk_core::Store, assigns: Vec<Assign>| -> Result<Value> {
        store.write(&actor, dry_run, |c| {
            let id = c.create(NewIssue {
                project: project.clone(),
                ty: ty.clone(),
                assigns,
                deps: new_deps.clone(),
            })?;
            c.show(
                &id,
                With {
                    deps: true,
                    ..Default::default()
                },
            )
        })
    };
    match attempt(&mut store, assigns.clone()) {
        Ok(v) => Ok(Out::Issue(v)),
        Err(e) if e.code() == Code::RequirementsUnmet && editor::is_tty() && !dry_run => {
            let schema = store.schema()?;
            let t = schema.ty(&ty)?.clone();
            let given: std::collections::BTreeMap<String, Value> = assigns
                .iter()
                .map(|a| (a.field.clone(), a.value.clone()))
                .collect();
            let mut doc_fields = vec![(
                "title".to_string(),
                given.get("title").cloned().unwrap_or(Value::Null),
                true,
                "required".to_string(),
            )];
            for tf in &t.fields {
                let hint = schema.field(&tf.field).map(|d| {
                    let mut h = d.kind.as_str().to_string();
                    if !d.enum_values().is_empty() {
                        h = format!("one of {}", d.enum_values().join(", "));
                    }
                    if tf.required_at_create {
                        h = format!("required; {h}");
                    }
                    h
                });
                doc_fields.push((
                    tf.field.clone(),
                    given.get(&tf.field).cloned().unwrap_or(Value::Null),
                    tf.required_at_create,
                    hint.unwrap_or_default(),
                ));
            }
            let context = given
                .get("context")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let context = if context.is_empty() {
                "<!-- context: background, motivation, constraints -->\n".to_string()
            } else {
                context
            };
            let v = editor::edit_loop(
                |errs| editor::document(errs, &doc_fields, &context),
                |a| {
                    let a: Vec<Assign> = a
                        .into_iter()
                        .filter(|x| {
                            !(x.field == "context"
                                && x.value.as_str().is_some_and(|s| {
                                    s.trim_start().starts_with("<!--")
                                        && s.trim_end().ends_with("-->")
                                }))
                        })
                        .collect();
                    attempt(&mut store, a)
                },
            )?;
            Ok(Out::Issue(v))
        }
        Err(e) => Err(e),
    }
}

fn edit(env: &Env, id: String, f: FieldArgs, from: Option<String>, dry_run: bool) -> Result<Out> {
    let actor = env.actor()?.to_string();
    let mut store = env.store()?;
    let mut assigns = vec![];
    if let Some(p) = &from {
        assigns.extend(from_file(p)?);
    }
    assigns.extend(parse_fields(&f)?);
    let apply = |store: &mut trk_core::Store, assigns: Vec<Assign>| -> Result<Value> {
        store.write(&actor, dry_run, |c| {
            let id = c.resolve_id(&id)?;
            c.edit(&id, &assigns)?;
            c.show(
                &id,
                With {
                    deps: true,
                    ..Default::default()
                },
            )
        })
    };
    if assigns.is_empty() {
        if !editor::is_tty() {
            return Err(bad_args("nothing to change").with_hint("pass -f k=v or --from FILE"));
        }
        let (cur, schema) = (
            store.read(None, |c| c.show(&id, With::default()))?,
            store.schema()?,
        );
        let t = schema.ty(cur["type"].as_str().unwrap_or_default())?.clone();
        let mut doc_fields = vec![(
            "title".to_string(),
            cur["title"].clone(),
            true,
            String::new(),
        )];
        for k in ["outcome", "owner", "tags"] {
            if !cur[k].is_null() && cur[k] != json!([]) {
                doc_fields.push((k.to_string(), cur[k].clone(), false, String::new()));
            } else {
                doc_fields.push((k.to_string(), Value::Null, false, String::new()));
            }
        }
        for tf in &t.fields {
            doc_fields.push((
                tf.field.clone(),
                cur["fields"][&tf.field].clone(),
                false,
                String::new(),
            ));
        }
        let context = cur["context"].as_str().unwrap_or("").to_string();
        let v = editor::edit_loop(
            |errs| editor::document(errs, &doc_fields, &context),
            |a| apply(&mut store, a),
        )?;
        return Ok(Out::Issue(v));
    }
    Ok(Out::Issue(apply(&mut store, assigns)?))
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
    let (applied, counts) = store.read(None, |c| Ok((c.last_schema_event()?, c.counts()?)))?;
    let data = json!({
        "version": env!("TRK_VERSION"),
        "actor": env.actor,
        "project": project,
        "repo": repo,
        "config": env.config.path,
        "config_commit": env.config.git_commit(),
        "db": env.db,
        "schema_applied": applied,
        "issues": counts,
    });
    Ok(Out::Data(data))
}

// ----------------------------------------------------------------------
// Agents
// ----------------------------------------------------------------------

fn agent_cmd(env: &Env, cmd: AgentCmd) -> Result<Out> {
    let store = env.store()?;
    let schema = store.schema()?;
    match cmd {
        AgentCmd::List => {
            let agents = trk_agent::load_agents(&env.config)?;
            let mut items = vec![];
            let mut text = String::new();
            for a in agents.values() {
                let types: Vec<String> = trk_agent::check::routed_types(&schema, &a.name)
                    .iter()
                    .map(|t| t.name.clone())
                    .collect();
                text.push_str(&format!(
                    "{:<12} {}\n             types: {}  harness: {}  builtin_tools: {}\n",
                    a.name,
                    a.description.clone().unwrap_or_default(),
                    if types.is_empty() {
                        "-".into()
                    } else {
                        types.join(", ")
                    },
                    a.harness.prefer.join(" > "),
                    a.harness.builtin_tools
                ));
                items.push(json!({"name": a.name, "description": a.description, "types": types,
                                  "harness": a.harness.prefer, "builtin_tools": a.harness.builtin_tools}));
            }
            Ok(Out::Text(text, Value::Array(items)))
        }
        AgentCmd::Preview { agent, ty, issue } => {
            let a = trk_agent::load_agent(&env.config, &agent)?;
            let (issue_json, t) = match (&issue, &ty) {
                (Some(id), _) => store.read(env.actor.as_deref(), |c| {
                    let v = c.show(
                        id,
                        With {
                            deps: true,
                            ..Default::default()
                        },
                    )?;
                    let t = c.schema.ty(v["type"].as_str().unwrap_or_default())?.clone();
                    Ok((v, t))
                })?,
                (None, Some(ty)) => {
                    let t = schema.ty(ty)?.clone();
                    (trk_agent::check::sample_issue(&schema, &t), t)
                }
                (None, None) => {
                    let t = trk_agent::check::routed_types(&schema, &a.name)
                        .first()
                        .map(|t| (*t).clone())
                        .ok_or_else(|| {
                            bad_args(format!("agent '{agent}' is not routed from any type"))
                                .with_hint("pass --type T or --issue ID")
                        })?;
                    (trk_agent::check::sample_issue(&schema, &t), t)
                }
            };
            let tools = trk_agent::agent_tools(&schema, &t, &a);
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
                "== instructions (MCP server) ==\n{instructions}\n\n== briefing (initial prompt) ==\n{briefing}\n\n== trk tools ({}) ==\n",
                t.name
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
                json!({"agent": a.name, "type": t.name, "instructions": instructions, "briefing": briefing, "tools": tool_json, "mcp": others}),
            ))
        }
    }
}

// ----------------------------------------------------------------------
// Schema
// ----------------------------------------------------------------------

fn parse_map(v: &[String]) -> Result<Vec<(String, String)>> {
    v.iter()
        .map(|s| {
            s.split_once('=')
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .ok_or_else(|| bad_args(format!("--map-status {s}: expected OLD=NEW")))
        })
        .collect()
}

fn diagnostics(env: &Env) -> Result<(trk_core::Schema, Vec<trk_core::config::Diagnostic>)> {
    let compiled = env.config.compile()?;
    let mut d = check_schema(&compiled);
    d.extend(trk_agent::check::check_agents(
        &env.config,
        &compiled,
        &env.user,
    ));
    Ok((compiled, d))
}

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

fn schema_apply(env: &Env, map: &[(String, String)]) -> Result<Out> {
    let actor = env.actor()?.to_string();
    let (compiled, diags) = diagnostics(env)?;
    let harnesses: Vec<String> = env.user.harness.keys().cloned().collect();
    let commit = env.config.git_commit();
    let mut store = env.store()?;
    let old = store.schema()?;
    let agents = trk_agent::load_agents(&env.config).unwrap_or_default();
    let plan = store.write(&actor, false, |c| {
        c.apply(&compiled, &harnesses, map, commit.clone())
    })?;
    let tools = trk_agent::check::tool_diff(&old, &compiled, &agents);
    let warnings: Vec<_> = diags
        .iter()
        .filter(|d| {
            d.level == Level::Warning || d.file.as_deref().is_some_and(|f| f.starts_with("agents/"))
        })
        .cloned()
        .collect();
    let mut text = format!(
        "schema applied: {} change(s){}",
        plan.changes.len(),
        commit
            .as_deref()
            .map(|c| format!(" (config {c})"))
            .unwrap_or_default()
    );
    if !warnings.is_empty() {
        text.push_str(&format!("\n{}", diag_text(&warnings)));
    }
    Ok(Out::Text(
        text,
        json!({"changes": plan.changes, "affected": plan.affected, "tools": tools, "commit": commit, "warnings": warnings}),
    ))
}

fn schema_cmd(env: &Env, cmd: Option<SchemaCmd>, ty: Option<String>) -> Result<Out> {
    match cmd {
        Some(SchemaCmd::Check) => {
            let (_, d) = diagnostics(env)?;
            let errors = d.iter().filter(|x| x.level == Level::Error).count();
            if errors > 0 {
                return Err(Error::new(
                    Code::SchemaInvalid,
                    format!("{errors} error(s) in {}", env.config.path.display()),
                )
                .with_detail(
                    "problems",
                    json!(
                        d.iter()
                            .map(|x| format!(
                                "{}: {}{}",
                                match x.level {
                                    Level::Error => "error",
                                    Level::Warning => "warning",
                                },
                                x.file
                                    .as_ref()
                                    .map(|f| format!("{f}: "))
                                    .unwrap_or_default(),
                                x.message
                            ))
                            .collect::<Vec<_>>()
                    ),
                )
                .with_detail("diagnostics", serde_json::to_value(&d)?));
            }
            let text = if d.is_empty() {
                "config OK".to_string()
            } else {
                format!("config OK\n{}", diag_text(&d))
            };
            Ok(Out::Text(text, json!({"ok": true, "diagnostics": d})))
        }
        Some(SchemaCmd::Plan { map_status }) => {
            let map = parse_map(&map_status)?;
            let (compiled, diags) = diagnostics(env)?;
            let store = env.store()?;
            let old = store.schema()?;
            let plan = store.read(env.actor.as_deref(), |c| c.plan(&compiled, &map))?;
            let agents = trk_agent::load_agents(&env.config).unwrap_or_default();
            let tools = trk_agent::check::tool_diff(&old, &compiled, &agents);
            let mut text = String::new();
            if plan.changes.is_empty() {
                text.push_str("registry: no changes\n");
            } else {
                text.push_str("registry:\n");
                for c in &plan.changes {
                    let sign = match c.op {
                        "add" => "+",
                        "remove" => "-",
                        _ => "~",
                    };
                    text.push_str(&format!(
                        "  {sign} {}{}\n",
                        c.what,
                        c.detail
                            .as_ref()
                            .map(|d| format!("  ({d})"))
                            .unwrap_or_default()
                    ));
                }
            }
            if !plan.affected.is_empty() {
                text.push_str("affected issues:\n");
                for a in &plan.affected {
                    text.push_str(&format!(
                        "  {}  {}\n",
                        a["id"].as_str().unwrap_or(""),
                        a["reason"].as_str().unwrap_or("")
                    ));
                }
            }
            if !tools.is_empty() {
                text.push_str("agent tools:\n");
                for t in &tools {
                    text.push_str(&format!(
                        "  {} on {}:",
                        t["agent"].as_str().unwrap_or(""),
                        t["type"].as_str().unwrap_or("")
                    ));
                    for (k, sign) in [("added", "+"), ("removed", "-"), ("changed", "~")] {
                        for n in t[k].as_array().into_iter().flatten() {
                            text.push_str(&format!(" {sign}{}", n.as_str().unwrap_or("")));
                        }
                    }
                    text.push('\n');
                }
            }
            if !plan.blockers.is_empty() {
                text.push_str("cannot apply:\n");
                for b in &plan.blockers {
                    text.push_str(&format!("  ✗ {b}\n"));
                }
            }
            let warns: Vec<_> = diags
                .iter()
                .filter(|d| {
                    d.level == Level::Warning
                        || d.file.as_deref().is_some_and(|f| f.starts_with("agents/"))
                })
                .cloned()
                .collect();
            if !warns.is_empty() {
                text.push_str(&format!("{}\n", diag_text(&warns)));
            }
            Ok(Out::Text(
                text,
                json!({"changes": plan.changes, "affected": plan.affected, "tools": tools, "blockers": plan.blockers, "diagnostics": diags}),
            ))
        }
        Some(SchemaCmd::Apply { map_status }) => schema_apply(env, &parse_map(&map_status)?),
        Some(SchemaCmd::Graph { ty, dot }) => {
            let store = env.store()?;
            let schema = store.schema()?;
            schema.require_applied()?;
            let t = schema.ty(&ty)?;
            let mut s = String::new();
            let first = t.first_active().map(|x| x.name.clone());
            let open = t.open_status().map(|x| x.name.clone()).unwrap_or_default();
            if dot {
                s.push_str(&format!("digraph \"{}\" {{\n  rankdir=LR;\n", t.name));
                for st in &t.statuses {
                    let shape = match st.category {
                        Category::Open => "circle",
                        Category::Active => "box",
                        Category::Closed => "doublecircle",
                    };
                    s.push_str(&format!("  \"{}\" [shape={shape}];\n", st.name));
                }
                if let Some(f) = &first {
                    s.push_str(&format!(
                        "  \"{open}\" -> \"{f}\" [label=\"claim\", style=dashed];\n"
                    ));
                    for st in t.statuses.iter().filter(|x| x.category == Category::Active) {
                        s.push_str(&format!(
                            "  \"{}\" -> \"{open}\" [label=\"unclaim\", style=dotted];\n",
                            st.name
                        ));
                    }
                }
                for (from, tos) in &t.transitions {
                    for to in tos {
                        if Some(to) == first.as_ref() && *from == open {
                            continue;
                        }
                        s.push_str(&format!("  \"{from}\" -> \"{to}\";\n"));
                    }
                }
                s.push_str("}\n");
            } else {
                s.push_str("stateDiagram-v2\n");
                s.push_str(&format!("  [*] --> {}\n", open));
                if let Some(f) = &first {
                    s.push_str(&format!("  {open} --> {f} : claim\n"));
                    for st in t.statuses.iter().filter(|x| x.category == Category::Active) {
                        s.push_str(&format!("  {} --> {open} : unclaim\n", st.name));
                    }
                }
                for (from, tos) in &t.transitions {
                    for to in tos {
                        if Some(to) == first.as_ref() && *from == open {
                            continue;
                        }
                        let label = if t.category_of(to) == Some(Category::Closed) {
                            format!(" : {}", t.resolutions.join(" | "))
                        } else {
                            String::new()
                        };
                        s.push_str(&format!("  {from} --> {to}{label}\n"));
                    }
                }
                if let Some(c) = t.closed_status() {
                    s.push_str(&format!("  {} --> [*]\n", c.name));
                }
            }
            Ok(Out::Text(
                s.clone(),
                json!({"type": t.name, "format": if dot { "dot" } else { "mermaid" }, "graph": s}),
            ))
        }
        Some(SchemaCmd::Index { field }) => {
            let store = env.store()?;
            let name = store.index_field(&field)?;
            Ok(Out::Text(
                format!("index {name} ready"),
                json!({"index": name}),
            ))
        }
        None => {
            let store = env.store()?;
            let schema = store.schema()?;
            schema.require_applied()?;
            match ty {
                Some(t) => {
                    let td = schema.ty(&t)?;
                    let mut s = format!(
                        "{}{}\n",
                        td.name,
                        td.description
                            .as_ref()
                            .map(|d| format!(" — {d}"))
                            .unwrap_or_default()
                    );
                    s.push_str(&format!(
                        "  statuses:    {}\n",
                        td.statuses
                            .iter()
                            .map(|x| format!("{} ({})", x.name, x.category.as_str()))
                            .collect::<Vec<_>>()
                            .join(" → ")
                    ));
                    s.push_str(&format!("  resolutions: {}\n", td.resolutions.join(", ")));
                    s.push_str("  transitions:\n");
                    for (f, tos) in &td.transitions {
                        s.push_str(&format!("    {f} → {}\n", tos.join(", ")));
                    }
                    s.push_str("  fields:\n");
                    for tf in &td.fields {
                        let def = schema.field(&tf.field);
                        s.push_str(&format!(
                            "    {}{}  {}{}\n",
                            tf.field,
                            if tf.required_at_create { "*" } else { "" },
                            def.map(|d| d.kind.as_str()).unwrap_or("?"),
                            def.map(|d| d.enum_values())
                                .filter(|v| !v.is_empty())
                                .map(|v| format!(" [{}]", v.join("|")))
                                .unwrap_or_default()
                        ));
                    }
                    s.push_str("  requirements:\n");
                    for r in &td.requirements {
                        s.push_str(&format!("    {}\n", r.describe()));
                    }
                    if let Some(a) = &td.agent {
                        s.push_str(&format!("  agent: {a}\n"));
                    }
                    Ok(Out::Text(s, serde_json::to_value(td)?))
                }
                None => {
                    let mut s = String::new();
                    for t in schema.types.values() {
                        s.push_str(&format!(
                            "{:<12} {}  [{}]\n",
                            t.name,
                            t.statuses
                                .iter()
                                .map(|x| x.name.as_str())
                                .collect::<Vec<_>>()
                                .join(" → "),
                            t.resolutions.join(", ")
                        ));
                    }
                    s.push_str(&format!(
                        "\ndependency kinds: {}\n",
                        schema
                            .dep_kinds
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                    Ok(Out::Text(s, serde_json::to_value(&schema)?))
                }
            }
        }
    }
}
