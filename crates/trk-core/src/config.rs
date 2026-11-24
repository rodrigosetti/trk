//! The project's `.trk/` directory and the settings in its `config.toml`.
//! `trk install` writes every setting explicitly. New settings default in older configs.
//!
//! ```text
//! config.toml      settings, launcher templates, harness profiles, views
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
    pub issues: IssuesSection,
    pub claims: ClaimsSection,
    pub check: CheckSection,
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
pub struct IssuesSection {
    /// Prefix of new issue ids: `<prefix>-<base32>`.
    pub prefix: String,
    /// Type assigned to issues created without an explicit type.
    #[serde(default = "default_type")]
    pub default_type: String,
}

impl Default for IssuesSection {
    fn default() -> Self {
        IssuesSection {
            prefix: "trk".into(),
            default_type: default_type(),
        }
    }
}

fn default_type() -> String {
    crate::seed::DEFAULT_TYPE.into()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaimsSection {
    /// Lease for agent claims, e.g. "30m".
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
    pub interval: String,
    /// How long one check may run before it counts as an error, e.g. "10m".
    pub timeout: String,
    /// Shell command for verifying the merged tree before local landing.
    #[serde(default)]
    pub verify: String,
    /// Maximum failed-check wakes per session; zero disables waking.
    #[serde(default = "default_max_wakes")]
    pub max_wakes: u32,
    /// Sweep closed, done issues after each full check pass.
    #[serde(default = "default_check_gc")]
    pub gc: bool,
    /// Minimum time closed before automatic cleanup; zero means immediately.
    #[serde(default = "default_gc_grace")]
    pub gc_grace: String,
}

impl Default for CheckSection {
    fn default() -> Self {
        CheckSection {
            interval: default_check_interval(),
            timeout: default_check_timeout(),
            verify: String::new(),
            max_wakes: default_max_wakes(),
            gc: default_check_gc(),
            gc_grace: default_gc_grace(),
        }
    }
}

fn default_max_wakes() -> u32 {
    5
}

fn default_check_gc() -> bool {
    true
}

fn default_gc_grace() -> String {
    "1h".into()
}

fn default_check_interval() -> String {
    "5m".into()
}

fn default_check_timeout() -> String {
    "10m".into()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RunSection {
    /// Where `trk run` creates worktrees: absolute, `~/…`, or relative to the
    /// project root.
    pub worktrees: String,
    /// Branch new issue branches start from and checks land on. Missing in
    /// older configs; the CLI detects the repository's default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

impl Default for RunSection {
    fn default() -> Self {
        RunSection {
            worktrees: ".trk/worktrees".into(),
            base: None,
        }
    }
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
        .with_hint("run `trk install` to write the default config")
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
        if !p.is_file() {
            return Err(Error::new(
                Code::NotInstalled,
                format!("no trk config at {}", p.display()),
            )
            .with_hint("run `trk install` in the project's root directory"));
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
    fn legacy_issues_config_uses_builtin_default_type() {
        let issues: IssuesSection = toml::from_str("prefix = \"legacy\"").unwrap();
        assert_eq!(issues.prefix, "legacy");
        assert_eq!(issues.default_type, "code-change");
        assert_eq!(issues.default_type, IssuesSection::default().default_type);
    }

    #[test]
    fn issues_config_preserves_configured_default_type() {
        let issues: IssuesSection =
            toml::from_str("prefix = \"custom\"\ndefault_type = \"investigation\"").unwrap();
        assert_eq!(issues.default_type, "investigation");
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("5m").unwrap().as_secs(), 300);
        assert_eq!(fmt_duration(300), "5m");
        assert_eq!(fmt_duration(90), "90s");
        assert_eq!(fmt_duration(86400), "1d");
        assert!(parse_duration("soon").is_err());
    }
}
