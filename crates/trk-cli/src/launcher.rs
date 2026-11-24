//! Named terminal launcher profiles.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};

use serde::Deserialize;
use trk_agent::harness::subst;
use trk_core::error::{Code, Error, Result};

use crate::env::Env;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Launcher {
    #[serde(skip)]
    pub name: Option<String>,
    #[serde(rename = "name")]
    pub name_template: Option<String>,
    pub name_max: Option<usize>,
    pub name_chars: Option<String>,
    pub driver: Option<String>,
    pub available: Option<Vec<String>>,
    pub start: Option<Vec<String>>,
    pub attach: Option<Vec<String>>,
    pub send: Option<Vec<String>>,
    pub exists: Option<Vec<String>>,
    pub focus: Option<Vec<String>>,
    pub close: Option<Vec<String>>,
    pub rename: Option<Vec<String>>,
    pub send_delay: Option<u64>,
    pub send_retries: Option<u64>,
    pub send_retry_delay: Option<u64>,
}

fn invalid(reason: impl std::fmt::Display) -> Error {
    Error::new(
        Code::ConfigInvalid,
        format!("config.toml [launcher]: {reason}"),
    )
}

pub fn for_claim(env: &Env, name: Option<&str>) -> Result<Launcher> {
    let name = name
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid("claim.launcher must name a launcher profile"))?;
    let root = env
        .user
        .launcher
        .as_ref()
        .and_then(toml::Value::as_table)
        .ok_or_else(|| invalid("launcher must be a table of named profiles"))?;
    for (key, value) in root {
        if !matches!(
            key.as_str(),
            "use" | "send_delay" | "send_retries" | "send_retry_delay"
        ) && !value.is_table()
        {
            return Err(invalid(format!(
                "launcher.{key} must be a named profile table"
            )));
        }
    }
    let key = format!("launcher.{name}");
    let value = root
        .get(name)
        .ok_or_else(|| invalid(format!("missing {key}")))?;
    let mut profile: Launcher = value
        .clone()
        .try_into()
        .map_err(|error| invalid(format!("{key}: {error}")))?;
    profile.name = Some(name.to_owned());
    for (setting, target) in [
        ("send_delay", &mut profile.send_delay),
        ("send_retries", &mut profile.send_retries),
        ("send_retry_delay", &mut profile.send_retry_delay),
    ] {
        if let Some(value) = root.get(setting) {
            let default = value
                .clone()
                .try_into()
                .map_err(|error| invalid(format!("launcher.{setting}: {error}")))?;
            target.get_or_insert(default);
        }
    }
    if profile
        .driver
        .as_deref()
        .is_some_and(|driver| driver != "herdr")
    {
        return Err(invalid(format!("{key}.driver: unknown launcher driver")));
    }
    Ok(profile)
}

/// Select only at launch. Later operations never re-run availability selection.
pub fn choose(env: &Env) -> Result<Launcher> {
    let names = env
        .user
        .launcher
        .as_ref()
        .and_then(|value| value.get("use"))
        .ok_or_else(|| invalid("missing launcher.use"))?;
    let names: Vec<String> = names
        .clone()
        .try_into()
        .map_err(|error| invalid(format!("launcher.use: {error}")))?;
    for name in &names {
        let profile = for_claim(env, Some(name))?;
        if profile.is_available() {
            if !profile.is_herdr() && profile.start.as_ref().is_none_or(|start| start.is_empty()) {
                return Err(invalid(format!(
                    "launcher.{name}.start: missing nonempty start template"
                )));
            }
            return Ok(profile);
        }
    }
    Err(invalid("launcher.use: no available launcher").with_hint(format!(
        "profiles tried: {}; install one of these launchers (Herdr, hc, or tmux), edit [launcher] use in config.toml, or run `trk run --fg`",
        if names.is_empty() {
            "(none; [launcher] use is empty)".to_owned()
        } else {
            names.join(", ")
        }
    )))
}

impl Launcher {
    pub fn is_available(&self) -> bool {
        if let Some(probe) = &self.available {
            self.operation(Some(probe), "", None, false)
                .is_ok_and(|out| out.is_some_and(|out| out.0))
        } else if self.is_herdr() {
            crate::herdr::available().is_ok()
        } else {
            true
        }
    }

    pub fn terminal_name(&self, vars: &[(&str, &str)]) -> String {
        let template = self.name_template.as_deref().unwrap_or("trk-{id}");
        let allowed = self.name_chars.as_deref().unwrap_or("a-z0-9_-");
        let max = self.name_max.unwrap_or(32);
        let mut name = String::with_capacity(max.min(32));
        let mut count = 0;
        // Expand once, streaming directly into the sanitized, bounded name.
        for part in template.split_inclusive('}') {
            let substitution = part
                .strip_suffix('}')
                .and_then(|part| part.split_once('{'))
                .and_then(|(literal, key)| {
                    vars.iter()
                        .find(|(name, _)| *name == key)
                        .map(|(_, value)| (literal, *value))
                });
            let (literal, value) = substitution.unwrap_or(("", part));
            for c in literal.chars().chain(value.chars()) {
                if count == max {
                    return name;
                }
                let c = c.to_ascii_lowercase();
                name.push(if name_char_allowed(allowed, c) {
                    c
                } else {
                    '-'
                });
                count += 1;
            }
        }
        name
    }

    pub fn is_herdr(&self) -> bool {
        self.driver.as_deref() == Some("herdr")
    }

    pub fn operation(
        &self,
        template: Option<&[String]>,
        name: &str,
        text: Option<&str>,
        inherit: bool,
    ) -> Result<Option<(bool, String)>> {
        let Some(template) = template.filter(|t| !t.is_empty()) else {
            return Ok(None);
        };
        let vars = BTreeMap::from([
            ("name", name.to_owned()),
            ("text", text.unwrap_or_default().to_owned()),
            ("title", text.unwrap_or_default().to_owned()),
        ]);
        run_template(template, &vars, inherit).map(Some)
    }

    pub fn exists(&self, name: &str) -> Option<bool> {
        if let Some(template) = &self.exists {
            let (program, args) = template.split_first()?;
            let vars = BTreeMap::from([("name", name.to_owned())]);
            let status = Command::new(subst(program, &vars))
                .args(args.iter().map(|arg| subst(arg, &vars)))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .ok()?;
            return Some(status.success());
        }
        self.is_herdr()
            .then(|| crate::herdr::has_terminal(name).ok())
            .flatten()
    }

    pub fn focus(&self, name: &str) -> Result<bool> {
        if self.focus.is_some() {
            return self
                .operation(self.focus.as_deref(), name, None, false)
                .map(|result| result.is_some_and(|out| out.0));
        }
        if self.is_herdr() {
            return crate::herdr::focus(name).map_err(|reason| Error::new(Code::Internal, reason));
        }
        Ok(false)
    }

    pub fn rename(&self, name: &str, title: &str) -> Result<()> {
        if let Some((ok, out)) = self.operation(self.rename.as_deref(), name, Some(title), false)? {
            if !ok {
                return Err(Error::new(
                    Code::Internal,
                    format!("launcher rename failed: {out}"),
                ));
            }
        } else if self.is_herdr() {
            crate::herdr::mark_closed(name).map_err(|reason| Error::new(Code::Internal, reason))?;
        }
        Ok(())
    }
}

/// Character sets contain inclusive ranges and literals; a trailing '-' is literal.
fn name_char_allowed(spec: &str, c: char) -> bool {
    let mut chars = spec.chars().peekable();
    while let Some(start) = chars.next() {
        let end = if chars.peek() == Some(&'-') {
            chars.next();
            match chars.next() {
                Some(end) => end,
                None => return c == start || c == '-',
            }
        } else {
            start
        };
        if (start..=end).contains(&c) {
            return true;
        }
    }
    false
}

pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub text: String,
}

pub fn capture(tmpl: &[String], vars: &BTreeMap<&str, String>) -> Result<Output> {
    let argv: Vec<String> = tmpl.iter().map(|a| subst(a, vars)).collect();
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| invalid("empty launcher template"))?;
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| Error::new(Code::Internal, format!("cannot run {program}: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut text = stdout.trim().to_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(stderr.trim());
    }
    Ok(Output {
        success: out.status.success(),
        stdout,
        text,
    })
}

pub fn run_template(
    tmpl: &[String],
    vars: &BTreeMap<&str, String>,
    inherit: bool,
) -> Result<(bool, String)> {
    if !inherit {
        let out = capture(tmpl, vars)?;
        return Ok((out.success, out.text));
    }
    let argv: Vec<String> = tmpl.iter().map(|a| subst(a, vars)).collect();
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| invalid("empty launcher template"))?;
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|e| Error::new(Code::Internal, format!("cannot run {program}: {e}")))?;
    Ok((status.success(), String::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_terminal_names_are_sanitized_and_bounded() {
        let launcher = Launcher::default();
        for (id, expected) in [
            ("orch-4kq", "trk-orch-4kq"),
            ("ABC_09-!é", "trk-abc_09---"),
            (
                "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
                "trk-abcdefghijklmnopqrstuvwxyz01",
            ),
        ] {
            assert_eq!(launcher.terminal_name(&[("id", id)]), expected);
        }
    }

    #[test]
    fn terminal_name_limits_characters_and_honors_custom_ranges_and_literals() {
        let launcher: Launcher = toml::from_str(
            r#"
name = "{project}.{id}_!"
name_max = 9
name_chars = "a-c0-2._é-"
"#,
        )
        .unwrap();
        assert_eq!(
            launcher.terminal_name(&[("project", "AéD"), ("id", "0123XYZ")]),
            "aé-.012--"
        );
    }

    #[test]
    fn terminal_name_does_not_expand_substituted_values() {
        let launcher: Launcher = toml::from_str(
            r#"
name = "{project}-{id}"
name_chars = "a-z0-9_{}-"
"#,
        )
        .unwrap();
        assert_eq!(
            launcher.terminal_name(&[("project", "{id}"), ("id", "abc")]),
            "{id}-abc"
        );
    }
}
