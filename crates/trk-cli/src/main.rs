//! `trk`: a local issue tracker and agent launcher.

mod check;
mod commands;
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

use std::io::IsTerminal;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use trk_core::error::Error;

use crate::render::{Format, Renderer};

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
        /// Existing active epic; omit when none fits
        #[arg(long)]
        epic: Option<String>,
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
    /// Change authored fields; $EDITOR with no options
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
        /// Existing active epic
        #[arg(long, conflicts_with = "clear_epic")]
        epic: Option<String>,
        /// Remove the issue's epic
        #[arg(long)]
        clear_epic: bool,
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
    /// Manage explicitly curated issue groupings
    Epic {
        #[command(subcommand)]
        cmd: EpicCmd,
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
    /// Timeline of one issue, or all events including epic changes
    Log { id: Option<String> },
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
        /// Recover unnamed historical hc sessions (requires explicit issue IDs and verified provenance)
        #[arg(long, requires = "ids")]
        legacy_hc: bool,
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
        after_help = "`trk herdr install` installs both mods and the trk layout without activating it.\n\
        Activate: herdr-app do layout.switch --name trk; revert: --name main.\n\
        The trk layout adds a fixed first dashboard tab to tracked primary/non-Git spaces.\n\
        `trk herdr open` instead opens an ordinary, closable project panel as a tab or split."
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
enum EpicCmd {
    /// Explicitly create an epic (any actor)
    New {
        name: String,
        /// Description: text, @file, or @- for stdin
        #[arg(short = 'd', long)]
        description: Option<String>,
    },
    /// List active epics with descriptions and total issue counts
    Ls {
        /// Include archived epics
        #[arg(long)]
        all: bool,
    },
    /// Rename an epic and cascade existing issue references
    Rename { old: String, new: String },
    /// Retain existing members but reject new assignments
    Archive { name: String },
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
    /// Install or refresh both mods and the trk layout (activate with layout.switch --name trk)
    Install {
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
    /// Remove both mods and the unedited trk layout; preserve personal overrides
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
        let code = match commands::mcp(&g, issue, actor, agent) {
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
    match commands::dispatch(&g, cli.cmd, completed, &r) {
        Ok(out) => r.emit(out),
        Err(e) => {
            r.error(&e);
            std::process::exit(e.exit_code());
        }
    }
}

