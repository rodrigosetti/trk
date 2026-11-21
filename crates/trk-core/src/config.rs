//! The config directory and the user settings in `config.toml`.
//!
//! ```text
//! config.toml      user settings, launcher templates, harness profiles, views
//! agents/*.toml    one file per agent (parsed by trk-agent)
//! prompts/*.j2     templates (rendered by trk-agent)
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Code, Error, Result};

#[derive(Debug, Clone)]
pub struct ConfigDir {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub level: Level,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warning,
}

impl Diagnostic {
    pub fn error(file: Option<&str>, message: impl Into<String>) -> Self {
        Diagnostic {
            level: Level::Error,
            file: file.map(String::from),
            message: message.into(),
        }
    }
    pub fn warning(file: Option<&str>, message: impl Into<String>) -> Self {
        Diagnostic {
            level: Level::Warning,
            file: file.map(String::from),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct UserConfig {
    #[serde(default)]
    pub user: UserSection,
    #[serde(default)]
    pub claims: ClaimsSection,
    #[serde(default)]
    pub check: CheckSection,
    #[serde(default)]
    pub run: RunSection,
    /// Launcher templates; parsed by trk-cli.
    #[serde(default)]
    pub launcher: Option<toml::Value>,
    /// Harness profiles keyed by harness name; parsed by trk-agent.
    #[serde(default)]
    pub harness: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub views: BTreeMap<String, View>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct UserSection {
    pub email: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaimsSection {
    /// Default lease for agent claims, e.g. "30m".
    #[serde(default = "default_lease")]
    pub agent_lease: String,
}

impl Default for ClaimsSection {
    fn default() -> Self {
        ClaimsSection {
            agent_lease: default_lease(),
        }
    }
}

fn default_lease() -> String {
    "30m".into()
}

/// Settings for waiting checks and `trk check`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CheckSection {
    /// How often a waiting issue is checked when it does not say, e.g. "5m".
    #[serde(default = "default_check_interval")]
    pub interval: String,
    /// How long one check may run before it counts as an error, e.g. "10m".
    #[serde(default = "default_check_timeout")]
    pub timeout: String,
}

impl Default for CheckSection {
    fn default() -> Self {
        CheckSection {
            interval: default_check_interval(),
            timeout: default_check_timeout(),
        }
    }
}

fn default_check_interval() -> String {
    "5m".into()
}

fn default_check_timeout() -> String {
    "10m".into()
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct RunSection {
    /// Where `trk run` creates worktrees; default `<data dir>/worktrees`.
    pub worktrees: Option<String>,
    /// Installed-harness preferences remain global.
    #[serde(default)]
    pub harness_prefer: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct View {
    pub query: String,
    #[serde(default)]
    pub fields: Option<Vec<String>>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

pub fn toml_to_json(v: &toml::Value) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| {
        Error::new(
            Code::NotFound,
            format!("cannot read {}: {e}", path.display()),
        )
        .with_hint("run `trk init` to write the default config")
    })
}

fn parse_toml<T: serde::de::DeserializeOwned>(path: &Path, text: &str) -> Result<T> {
    toml::from_str(text)
        .map_err(|e| Error::new(Code::ConfigInvalid, format!("{}: {e}", path.display())))
}

impl ConfigDir {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        ConfigDir { path: path.into() }
    }

    pub fn exists(&self) -> bool {
        self.path.join("config.toml").is_file()
    }

    pub fn user_config(&self) -> Result<UserConfig> {
        let p = self.path.join("config.toml");
        if !p.exists() {
            return Ok(UserConfig::default());
        }
        parse_toml(&p, &read(&p)?)
    }

    /// Files in a subdirectory with the given extension, sorted.
    pub fn files_in(&self, sub: &str, ext: &str) -> Vec<PathBuf> {
        let mut out = vec![];
        if let Ok(rd) = std::fs::read_dir(self.path.join(sub)) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some(ext) {
                    out.push(p);
                }
            }
        }
        out.sort();
        out
    }

    /// The git commit of the config directory, with `+dirty` when it has
    /// uncommitted changes. `None` when it is not a git repository.
    pub fn git_commit(&self) -> Option<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(["rev-parse", "HEAD"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let mut commit = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let dirty = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(["status", "--porcelain"])
            .output()
            .ok()
            .is_some_and(|o| !o.stdout.is_empty());
        if dirty {
            commit.push_str("+dirty");
        }
        Some(commit)
    }
}

/// Parse a duration like `30m`, `2h`, `90s`, `1d`.
pub fn parse_duration(s: &str) -> Result<jiff::SignedDuration> {
    let s = s.trim();
    let bad = || {
        Error::new(
            Code::BadArgs,
            format!("invalid duration '{s}' (expected e.g. 30m, 2h, 90s, 1d)"),
        )
    };
    if s.is_empty() {
        return Err(bad());
    }
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: i64 = num.parse().map_err(|_| bad())?;
    let secs = match unit {
        "s" | "" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        "w" => n * 7 * 86400,
        _ => return Err(bad()),
    };
    Ok(jiff::SignedDuration::from_secs(secs))
}

/// The shortest exact rendering of a number of seconds: `90s`, `5m`, `2h`, `1d`.
pub fn fmt_duration(secs: i64) -> String {
    for (unit, n) in [("d", 86400), ("h", 3600), ("m", 60)] {
        if secs >= n && secs % n == 0 {
            return format!("{}{unit}", secs / n);
        }
    }
    format!("{secs}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("5m").unwrap().as_secs(), 300);
        assert_eq!(fmt_duration(300), "5m");
        assert_eq!(fmt_duration(90), "90s");
        assert_eq!(fmt_duration(86400), "1d");
        assert!(parse_duration("soon").is_err());
    }
}
