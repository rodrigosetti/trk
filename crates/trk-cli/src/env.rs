//! Resolving the environment: the project's `.trk/` directory, the acting
//! actor, and opening the store.
//!
//! Everything trk keeps lives in `<project root>/.trk/`, created by
//! `trk install`:
//!
//! ```text
//! .trk/trk.db                         the database
//! .trk/config.toml agents/ prompts/ types/
//! .trk/worktrees/<id>/                `trk run` worktrees (configurable)
//! .trk/sessions/<uuid>/  .trk/checks/ session files and check logs
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use trk_core::config::{ConfigDir, UserConfig, parse_duration};
use trk_core::error::{Code, Error, Result};
use trk_core::{Store, StoreOpts};

use crate::Global;

pub const DIR_NAME: &str = ".trk";
pub const DB_NAME: &str = "trk.db";

pub struct Env {
    /// The project root: the directory that contains `.trk/`.
    pub root: PathBuf,
    /// `<root>/.trk`.
    pub dir: PathBuf,
    pub db: PathBuf,
    pub config: ConfigDir,
    pub user: UserConfig,
    pub actor: Option<String>,
    /// The git checkout of the working directory, if any.
    pub git: Option<GitInfo>,
}

#[derive(Debug, Clone)]
pub struct GitInfo {
    /// The checkout the working directory is in (possibly a linked worktree).
    pub toplevel: PathBuf,
    /// The repository's primary checkout, shared by all its worktrees.
    pub primary: PathBuf,
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn expand_tilde(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(p),
    }
}

fn canonical(p: impl AsRef<Path>) -> PathBuf {
    let p = p.as_ref();
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

pub fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The checkout of `dir` and the primary checkout of its repository. The
/// primary checkout is the first entry of `git worktree list`; -z avoids
/// quoting paths that contain spaces or newlines.
pub fn git_info(dir: &Path) -> Option<GitInfo> {
    let toplevel = canonical(git(dir, &["rev-parse", "--show-toplevel"])?);
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()
        .ok()?;
    let primary = out
        .status
        .success()
        .then(|| {
            let first = out.stdout.split(|b| *b == 0).next()?;
            let root = first.strip_prefix(b"worktree ")?;
            Some(canonical(String::from_utf8_lossy(root).into_owned()))
        })
        .flatten()
        .unwrap_or_else(|| toplevel.clone());
    Some(GitInfo { toplevel, primary })
}

/// Detect the landing branch from the repository's primary checkout.
pub fn detect_base(primary: &Path) -> String {
    git(
        primary,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    )
    .and_then(|r| r.strip_prefix("refs/remotes/origin/").map(String::from))
    .or_else(|| git(primary, &["symbolic-ref", "--quiet", "--short", "HEAD"]))
    .unwrap_or_else(|| "main".into())
}

/// `<git common dir>/info/exclude` for the repository at `dir`.
fn exclude_file(dir: &Path) -> Option<PathBuf> {
    let common = git(dir, &["rev-parse", "--git-common-dir"])?;
    let common = if Path::new(&common).is_absolute() {
        PathBuf::from(common)
    } else {
        dir.join(common)
    };
    Some(common.join("info/exclude"))
}

/// Add a pattern to the repository's `.git/info/exclude` (shared by its worktrees).
pub fn git_exclude(dir: &Path, pattern: &str) {
    let Some(ex) = exclude_file(dir) else { return };
    let current = std::fs::read_to_string(&ex).unwrap_or_default();
    if current.lines().any(|l| l == pattern) {
        return;
    }
    let _ = std::fs::create_dir_all(ex.parent().unwrap());
    let mut s = current;
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    s.push_str(pattern);
    s.push('\n');
    let _ = std::fs::write(&ex, s);
}

pub fn hostname() -> Option<String> {
    Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Where the project's `.trk/` is, or would be installed.
pub struct Location {
    pub root: PathBuf,
    pub dir: PathBuf,
    pub git: Option<GitInfo>,
}

impl Location {
    /// `--dir` / `TRK_DIR` when given. Otherwise the nearest ancestor of the
    /// working directory with a `.trk/trk.db`, then the primary checkout of
    /// the working directory's git repository (a `trk run` worktree does not
    /// contain `.trk/`), then the working directory itself.
    pub fn find(g: &Global) -> Location {
        let cwd = canonical(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let git = git_info(&cwd);
        if let Some(dir) = &g.dir {
            let dir = if dir.is_absolute() {
                dir.clone()
            } else {
                cwd.join(dir)
            };
            let root = dir.parent().map(Path::to_path_buf).unwrap_or(cwd);
            return Location { root, dir, git };
        }
        let root = cwd
            .ancestors()
            .find(|a| a.join(DIR_NAME).join(DB_NAME).is_file())
            .map(Path::to_path_buf)
            .or_else(|| git.as_ref().map(|g| g.primary.clone()))
            .unwrap_or(cwd);
        Location {
            dir: root.join(DIR_NAME),
            root,
            git,
        }
    }

    pub fn db(&self) -> PathBuf {
        self.dir.join(DB_NAME)
    }

    pub fn installed(&self) -> bool {
        self.db().is_file() && self.dir.join("config.toml").is_file()
    }

    pub fn not_installed(&self) -> Error {
        Error::new(
            Code::NotInstalled,
            format!("trk is not installed in {}", self.root.display()),
        )
        .with_hint("run `trk install` in the project's root directory")
    }
}

impl Env {
    /// The environment of an installed project. Fails with `not_installed`
    /// when there is no `.trk/` with a database and a config.
    pub fn new(g: &Global) -> Result<Env> {
        let loc = Location::find(g);
        if !loc.installed() {
            return Err(loc.not_installed());
        }
        Env::at(loc, g)
    }

    pub fn at(loc: Location, g: &Global) -> Result<Env> {
        let config = ConfigDir::new(&loc.dir);
        let user = config.user_config()?;
        let actor = match &g.actor {
            Some(a) => Some(a.clone()),
            None => user.user.email.as_ref().map(|e| format!("human:{e}")),
        };
        Ok(Env {
            db: loc.db(),
            root: loc.root,
            dir: loc.dir,
            config,
            user,
            actor,
            git: loc.git,
        })
    }

    pub fn actor(&self) -> Result<&str> {
        self.actor.as_deref().ok_or_else(|| {
            Error::new(Code::BadArgs, "no acting actor").with_hint(
                "set [user] email in .trk/config.toml, TRK_ACTOR, or pass --as human:<email>",
            )
        })
    }

    pub fn agent_lease(&self) -> Result<jiff::SignedDuration> {
        parse_duration(&self.user.claims.agent_lease)
    }

    pub fn store_opts(&self) -> Result<StoreOpts> {
        Ok(StoreOpts {
            now: std::env::var("TRK_NOW").ok().and_then(|s| s.parse().ok()),
            agent_lease: self.agent_lease()?,
            check_interval: parse_duration(&self.user.check.interval)?,
            max_wakes: self.user.check.max_wakes,
            prefix: self.user.issues.prefix.clone(),
        })
    }

    pub fn store(&self) -> Result<Store> {
        Store::open(&self.db, self.store_opts()?)
    }

    /// The project's git checkout, when its root is one: where `trk run`
    /// makes worktrees from, and `TRK_REPO` for checks.
    pub fn checkout(&self) -> Option<PathBuf> {
        self.root.join(".git").exists().then(|| self.root.clone())
    }

    /// `[run] base`, with repository detection for older configs.
    pub fn base(&self) -> String {
        self.user
            .run
            .base
            .clone()
            .unwrap_or_else(|| detect_base(&self.root))
    }

    /// The checkout of the working directory, when it belongs to this
    /// project: the project root or one of its worktrees.
    pub fn here(&self) -> Option<PathBuf> {
        let g = self.git.as_ref()?;
        (g.primary == canonical(&self.root)).then(|| g.toplevel.clone())
    }

    pub fn check_timeout(&self) -> Result<std::time::Duration> {
        let d = parse_duration(&self.user.check.timeout)?;
        Ok(std::time::Duration::from_secs(d.as_secs().max(1) as u64))
    }

    /// `[run] worktrees`: absolute, `~/…`, or relative to the project root.
    pub fn worktrees_dir(&self) -> PathBuf {
        let p = expand_tilde(&self.user.run.worktrees);
        if p.is_absolute() {
            p
        } else {
            self.root.join(p)
        }
    }

    /// Per-session files written by `trk run`.
    pub fn sessions_dir(&self) -> PathBuf {
        self.dir.join("sessions")
    }

    /// Logs of running checks.
    pub fn checks_dir(&self) -> PathBuf {
        self.dir.join("checks")
    }

    /// `TRK_DIR` for child processes, so they reach this project from any
    /// working directory.
    pub fn dir_env(&self) -> String {
        canonical(&self.dir).to_string_lossy().to_string()
    }
}
