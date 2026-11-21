//! Resolving the environment: paths, the acting actor, the current repo and
//! project, and opening the store.

use std::path::{Path, PathBuf};
use std::process::Command;

use trk_core::config::{ConfigDir, UserConfig, parse_duration};
use trk_core::error::{Code, Error, Result};
use trk_core::model::normalize_remote;
use trk_core::{Store, StoreOpts};

use crate::Global;

pub struct Env {
    pub db: PathBuf,
    pub config: ConfigDir,
    pub user: UserConfig,
    pub actor: Option<String>,
    pub explicit_project: Option<String>,
    pub git: Option<GitInfo>,
}

#[derive(Debug, Clone)]
pub struct GitInfo {
    pub toplevel: String,
    pub remote: Option<String>,
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn xdg(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

pub fn expand_tilde(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(p),
    }
}

pub fn default_db() -> PathBuf {
    if let Some(d) = xdg("XDG_DATA_HOME") {
        return d.join("trk/trk.db");
    }
    directories::ProjectDirs::from("", "", "trk")
        .map(|p| p.data_dir().join("trk.db"))
        .unwrap_or_else(|| home().join(".local/share/trk/trk.db"))
}

pub fn default_config() -> PathBuf {
    xdg("XDG_CONFIG_HOME")
        .unwrap_or_else(|| home().join(".config"))
        .join("trk")
}

pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME")
        .unwrap_or_else(|| home().join(".local/state"))
        .join("trk")
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

/// `git rev-parse --show-toplevel` and `git remote get-url origin` in `dir`.
pub fn git_info(dir: &Path) -> Option<GitInfo> {
    let toplevel = git(dir, &["rev-parse", "--show-toplevel"])?;
    let remote = git(dir, &["remote", "get-url", "origin"]).filter(|s| !s.is_empty());
    let toplevel = std::fs::canonicalize(&toplevel)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or(toplevel);
    Some(GitInfo { toplevel, remote })
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

/// Remove a pattern added by [`git_exclude`].
pub fn git_unexclude(dir: &Path, pattern: &str) {
    let Some(ex) = exclude_file(dir) else { return };
    let Ok(current) = std::fs::read_to_string(&ex) else {
        return;
    };
    if !current.lines().any(|l| l == pattern) {
        return;
    }
    let kept: Vec<&str> = current.lines().filter(|l| *l != pattern).collect();
    let mut s = kept.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    let _ = std::fs::write(&ex, s);
}

pub fn hostname() -> Option<String> {
    Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

impl Env {
    pub fn new(g: &Global) -> Result<Env> {
        let db = g.db.clone().unwrap_or_else(default_db);
        let config = ConfigDir::new(g.config.clone().unwrap_or_else(default_config));
        let user = config.user_config()?;
        let actor = match &g.actor {
            Some(a) => Some(a.clone()),
            None => user
                .user
                .email
                .clone()
                .or_else(|| {
                    git(Path::new("."), &["config", "user.email"]).filter(|e| !e.is_empty())
                })
                .map(|e| format!("human:{e}")),
        };
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Ok(Env {
            db,
            config,
            user,
            actor,
            explicit_project: g.project.clone(),
            git: git_info(&cwd),
        })
    }

    pub fn actor(&self) -> Result<&str> {
        self.actor.as_deref().ok_or_else(|| {
            Error::new(Code::BadArgs, "no acting actor")
                .with_hint("set [user] email in config.toml, TRK_ACTOR, or pass --as human:<email>")
        })
    }

    pub fn agent_lease(&self) -> Result<jiff::SignedDuration> {
        parse_duration(&self.user.claims.agent_lease)
    }

    pub fn store(&self) -> Result<Store> {
        let now = std::env::var("TRK_NOW").ok().and_then(|s| s.parse().ok());
        let opts = StoreOpts {
            now,
            agent_lease: self.agent_lease()?,
            current_repo: None,
            check_interval: parse_duration(&self.user.check.interval)?,
            migration_config: Some(self.config.path.clone()),
        };
        let mut store = Store::open(&self.db, opts)?;
        store.opts.current_repo = self
            .current_repo(&store)?
            .and_then(|r| r.remote_url.or(r.local_path));
        Ok(store)
    }

    /// The registered repo of the working directory, if any.
    pub fn current_repo(&self, store: &Store) -> Result<Option<trk_core::model::Repo>> {
        let Some(g) = &self.git else { return Ok(None) };
        store.read(None, |c| {
            c.find_repo(g.remote.as_deref(), Some(&g.toplevel))
        })
    }

    /// The project: --project / TRK_PROJECT, else the working directory's repo.
    pub fn project(&self, store: &Store) -> Result<String> {
        if let Some(p) = &self.explicit_project {
            return Ok(p.clone());
        }
        match self.current_repo(store)? {
            Some(r) => Ok(r.project),
            None => Err(Error::new(
                Code::UnknownRepo,
                "the current directory is not a registered repository",
            )
            .with_hint(match &self.git {
                Some(g) => format!(
                    "trk repo add --project <id>   (registers {})",
                    g.remote
                        .as_deref()
                        .map(normalize_remote)
                        .unwrap_or_else(|| g.toplevel.clone())
                ),
                None => {
                    "pass --project, or run inside a git repository registered with `trk repo add`"
                        .into()
                }
            })),
        }
    }

    pub fn check_timeout(&self) -> Result<std::time::Duration> {
        let d = parse_duration(&self.user.check.timeout)?;
        Ok(std::time::Duration::from_secs(d.as_secs().max(1) as u64))
    }

    pub fn worktrees_dir(&self) -> PathBuf {
        match &self.user.run.worktrees {
            Some(w) => expand_tilde(w),
            None => self
                .db
                .parent()
                .map(|p| p.join("worktrees"))
                .unwrap_or_else(|| state_dir().join("worktrees")),
        }
    }
}
