//! Harness profiles and the launch spec. A profile says how to run an agent in
//! one harness: command, arguments, environment, files, and how MCP servers
//! and instructions are passed. Profiles never see issue data except through
//! placeholders.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use trk_core::config::UserConfig;
use trk_core::error::{Code, Error, Result};

use crate::agent::{AgentDef, McpServer};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessProfile {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Files written before launch: path (relative to the working directory,
    /// or absolute after placeholders) → content.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// Extra arguments per `builtin_tools` level (`read-only`, `none`).
    #[serde(default)]
    pub builtin_tools: BTreeMap<String, Vec<String>>,
    /// Whether the harness shows MCP server instructions to the model. When
    /// false, trk folds the instructions into the briefing.
    #[serde(default = "yes")]
    pub server_instructions: bool,
    /// How the briefing reaches the harness: "arg" (the `{briefing}`
    /// placeholder) or "send" (typed in with the launcher's send template).
    #[serde(default)]
    pub briefing: Option<String>,
}

fn yes() -> bool {
    true
}

impl HarnessProfile {
    pub fn sends_briefing(&self) -> bool {
        match self.briefing.as_deref() {
            Some("send") => true,
            Some(_) => false,
            None => !self.args.iter().any(|a| a.contains("{briefing}")),
        }
    }
}

pub fn profiles(user: &UserConfig) -> Result<BTreeMap<String, HarnessProfile>> {
    let mut out = BTreeMap::new();
    for (name, v) in &user.harness {
        let p: HarnessProfile = v.clone().try_into().map_err(|e| {
            Error::new(
                Code::ConfigInvalid,
                format!("config.toml [harness.{name}]: {e}"),
            )
        })?;
        out.insert(name.clone(), p);
    }
    Ok(out)
}

/// Is `cmd` an executable on PATH (or an existing path)?
pub fn installed(cmd: &str) -> bool {
    if cmd.contains('/') {
        return Path::new(cmd).exists();
    }
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

/// The harness for a run: `--harness`, else the agent's first installed
/// preference, else the first installed profile.
pub fn choose(
    agent: &AgentDef,
    profiles: &BTreeMap<String, HarnessProfile>,
    explicit: Option<&str>,
) -> Result<String> {
    if let Some(h) = explicit {
        if !profiles.contains_key(h) {
            return Err(
                Error::new(Code::NotFound, format!("no harness profile '{h}'")).with_hint(format!(
                    "profiles: {}",
                    profiles.keys().cloned().collect::<Vec<_>>().join(", ")
                )),
            );
        }
        return Ok(h.to_string());
    }
    for h in &agent.harness.prefer {
        if let Some(p) = profiles.get(h)
            && installed(&p.command)
        {
            return Ok(h.clone());
        }
    }
    for (h, p) in profiles {
        if installed(&p.command) {
            return Ok(h.clone());
        }
    }
    Err(Error::new(Code::NotFound, "no installed harness found")
        .with_hint("add a [harness.<name>] profile to config.toml whose command is on PATH"))
}

#[derive(Debug, Clone, Serialize)]
pub struct LaunchSpec {
    pub issue: String,
    pub agent: String,
    pub harness: String,
    pub actor: String,
    pub on_behalf_of: Option<String>,
    pub terminal: String,
    pub cwd: String,
    pub worktree: Option<String>,
    pub session_dir: String,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Absolute path → content.
    pub files: BTreeMap<String, String>,
    pub briefing: String,
    pub send_briefing: bool,
    pub instructions: String,
    pub mcp_servers: Value,
    pub shell_command: String,
}

pub struct LaunchInput<'a> {
    pub issue: &'a str,
    pub agent: &'a AgentDef,
    pub harness: &'a str,
    pub profile: &'a HarnessProfile,
    pub actor: &'a str,
    pub on_behalf_of: Option<&'a str>,
    pub terminal: &'a str,
    pub cwd: &'a str,
    pub worktree: Option<&'a str>,
    pub session_dir: &'a str,
    /// Absolute path of the trk binary the MCP server is started with.
    pub trk_bin: &'a str,
    /// TRK_DIR for the MCP server, so it reaches the project's database and config.
    pub trk_env: BTreeMap<String, String>,
    pub briefing: String,
    pub instructions: String,
    /// Names of the agent's trk tools, for `{trk_tools:PREFIX}`.
    pub tools: Vec<String>,
}

pub fn sh_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// `${env:NAME}` in agent MCP server env values comes from trk's environment.
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${env:") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 6..];
        match after.find('}') {
            Some(j) => {
                out.push_str(&std::env::var(&after[..j]).unwrap_or_default());
                rest = &after[j + 1..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// `{trk_tools:PREFIX}` → the agent's trk tool names, each with PREFIX,
/// comma-separated (e.g. `{trk_tools:mcp__trk_}` for omp's `--tools`).
pub fn expand_tool_lists(s: &str, tools: &[String]) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("{trk_tools:") {
        out.push_str(&rest[..i]);
        let after = &rest[i + "{trk_tools:".len()..];
        match after.find('}') {
            Some(j) => {
                let prefix = &after[..j];
                out.push_str(
                    &tools
                        .iter()
                        .map(|t| format!("{prefix}{t}"))
                        .collect::<Vec<_>>()
                        .join(","),
                );
                rest = &after[j + 1..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn subst(s: &str, vars: &BTreeMap<&str, String>) -> String {
    let mut out = s.to_string();
    for (k, v) in vars {
        let pat = format!("{{{k}}}");
        if out.contains(&pat) {
            out = out.replace(&pat, v);
        }
    }
    out
}

pub fn trk_mcp_args(issue: &str, actor: &str, agent: &str) -> Vec<String> {
    ["mcp", "--issue", issue, "--actor", actor, "--agent", agent]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

pub fn build(input: LaunchInput) -> Result<LaunchSpec> {
    let trk_args = trk_mcp_args(input.issue, input.actor, &input.agent.name);
    let others: Vec<&McpServer> = input.agent.mcp.iter().collect();

    // Claude Code / omp style: {"mcpServers": {...}}
    let mut servers = serde_json::Map::new();
    servers.insert(
        "trk".into(),
        json!({"command": input.trk_bin, "args": trk_args, "env": input.trk_env}),
    );
    for m in &others {
        let env: BTreeMap<String, String> = m
            .env
            .iter()
            .map(|(k, v)| (k.clone(), expand_env(v)))
            .collect();
        servers.insert(
            m.name.clone(),
            json!({"command": m.command, "args": m.args, "env": env}),
        );
    }
    let mcp_servers = json!({"mcpServers": servers});

    // OpenCode runtime config.
    let mut oc = serde_json::Map::new();
    let mut trk_cmd = vec![input.trk_bin.to_string()];
    trk_cmd.extend(trk_args.iter().cloned());
    oc.insert(
        "trk".into(),
        json!({"type": "local", "command": trk_cmd, "environment": input.trk_env, "enabled": true}),
    );
    for m in &others {
        let mut cmd = vec![m.command.clone()];
        cmd.extend(m.args.iter().cloned());
        let env: BTreeMap<String, String> = m
            .env
            .iter()
            .map(|(k, v)| (k.clone(), expand_env(v)))
            .collect();
        oc.insert(
            m.name.clone(),
            json!({"type": "local", "command": cmd, "environment": env, "enabled": true}),
        );
    }
    let opencode_json = json!({"$schema": "https://opencode.ai/config.json", "mcp": oc});

    let session_dir = input.session_dir.to_string();
    let mut vars: BTreeMap<&str, String> = BTreeMap::new();
    vars.insert("name", input.terminal.to_string());
    vars.insert("issue", input.issue.to_string());
    vars.insert("actor", input.actor.to_string());
    vars.insert("agent", input.agent.name.clone());
    vars.insert("harness", input.harness.to_string());
    vars.insert("cwd", input.cwd.to_string());
    vars.insert("worktree", input.worktree.unwrap_or(input.cwd).to_string());
    vars.insert("session_dir", session_dir.clone());
    vars.insert("briefing", input.briefing.clone());
    vars.insert("briefing_file", format!("{session_dir}/briefing.md"));
    vars.insert("instructions", input.instructions.clone());
    vars.insert(
        "instructions_file",
        format!("{session_dir}/instructions.md"),
    );
    vars.insert("mcp_json", serde_json::to_string(&mcp_servers)?);
    vars.insert("mcp_json_file", format!("{session_dir}/mcp.json"));
    vars.insert("opencode_json", serde_json::to_string(&opencode_json)?);
    vars.insert("trk_command", input.trk_bin.to_string());
    vars.insert(
        "trk_args_toml",
        format!(
            "[{}]",
            trk_args
                .iter()
                .map(|a| toml_str(a))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    vars.insert(
        "trk_env_toml",
        format!(
            "{{{}}}",
            input
                .trk_env
                .iter()
                .map(|(k, v)| format!("{k} = {}", toml_str(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );

    let level = input.agent.harness.builtin_tools.as_str();
    let extra: Vec<String> = if level == "default" {
        vec![]
    } else {
        input
            .profile
            .builtin_tools
            .get(level)
            .cloned()
            .unwrap_or_default()
    };
    let extra: Vec<String> = extra
        .iter()
        .map(|x| expand_tool_lists(x, &input.tools))
        .collect();
    let mut argv = vec![subst(&input.profile.command, &vars)];
    let pos = input
        .profile
        .args
        .iter()
        .position(|a| a.contains("{briefing}"))
        .unwrap_or(input.profile.args.len());
    for (i, a) in input.profile.args.iter().enumerate() {
        if i == pos {
            argv.extend(extra.iter().map(|x| subst(x, &vars)));
        }
        argv.push(subst(a, &vars));
    }
    if pos == input.profile.args.len() {
        argv.extend(extra.iter().map(|x| subst(x, &vars)));
    }

    let mut env: BTreeMap<String, String> = BTreeMap::new();
    env.insert("TRK_ACTOR".into(), input.actor.to_string());
    for (k, v) in &input.profile.env {
        env.insert(k.clone(), subst(v, &vars));
    }

    let mut files = BTreeMap::new();
    files.insert(format!("{session_dir}/briefing.md"), input.briefing.clone());
    files.insert(
        format!("{session_dir}/instructions.md"),
        input.instructions.clone(),
    );
    files.insert(
        format!("{session_dir}/mcp.json"),
        serde_json::to_string_pretty(&mcp_servers)?,
    );
    for (p, content) in &input.profile.files {
        let p = subst(p, &vars);
        let abs = if Path::new(&p).is_absolute() {
            p
        } else {
            format!("{}/{}", input.cwd, p)
        };
        files.insert(abs, subst(content, &vars));
    }

    let mut sh = format!("cd {} && exec env", sh_quote(input.cwd));
    for (k, v) in &env {
        sh.push_str(&format!(" {}={}", k, sh_quote(v)));
    }
    for a in &argv {
        sh.push(' ');
        sh.push_str(&sh_quote(a));
    }

    Ok(LaunchSpec {
        issue: input.issue.to_string(),
        agent: input.agent.name.clone(),
        harness: input.harness.to_string(),
        actor: input.actor.to_string(),
        on_behalf_of: input.on_behalf_of.map(String::from),
        terminal: input.terminal.to_string(),
        cwd: input.cwd.to_string(),
        worktree: input.worktree.map(String::from),
        session_dir,
        argv,
        env,
        files,
        briefing: input.briefing,
        send_briefing: input.profile.sends_briefing(),
        instructions: input.instructions,
        mcp_servers,
        shell_command: sh,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("abc"), "abc");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote(""), "''");
    }

    #[test]
    fn substitution() {
        let mut v = BTreeMap::new();
        v.insert("name", "trk-x".to_string());
        assert_eq!(subst("--name={name} {other}", &v), "--name=trk-x {other}");
    }
}
