//! Harness profiles and the launch spec. A profile says how to run an agent in
//! one harness: command, arguments, environment, files, and how MCP servers
//! and instructions are passed. Profiles render against a structured session
//! context; deprecated single-brace placeholders remain supported.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use trk_core::config::UserConfig;
use trk_core::error::{Code, Error, Result};

use crate::agent::AgentDef;

#[path = "harness_template.rs"]
mod template;
use template::{Renderer, uses_briefing};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessProfile {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Additional arguments prepended to `args`, rendered as a JSON string array.
    #[serde(default)]
    pub args_json: Option<String>,
    /// Default model; omitted leaves model selection to the harness.
    #[serde(default)]
    pub model: Option<String>,
    /// Added only when a model resolves, rendered with the session context.
    #[serde(default)]
    pub model_args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Files written before launch: path (relative to the working directory,
    /// or absolute after rendering) → content.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// Extra arguments per project-defined `builtin_tools` level; `default` is reserved.
    #[serde(default)]
    pub builtin_tools: BTreeMap<String, Vec<String>>,
    /// Whether the harness shows MCP server instructions to the model. When
    /// false, trk folds the instructions into the briefing.
    #[serde(default = "yes")]
    pub server_instructions: bool,
    /// How the briefing reaches the harness: "arg" (the `briefing` template
    /// variable) or "send" (typed in with the launcher's send template).
    #[serde(default)]
    pub briefing: Option<String>,
}

fn yes() -> bool {
    true
}

impl HarnessProfile {
    /// Resolve tool restrictions, refusing levels this harness cannot enforce.
    pub fn builtin_args(&self, level: &str, harness: &str) -> Result<Option<&Vec<String>>> {
        if level == "default" {
            return Ok(None);
        }
        self.builtin_tools.get(level).map(Some).ok_or_else(|| {
            Error::new(
                Code::ConfigInvalid,
                format!("harness '{harness}' cannot enforce builtin_tools = \"{level}\" (no builtin_tools.{level} in its profile)"),
            )
            .with_hint(format!(
                "define builtin_tools.{level} in [harness.{harness}] in config.toml, or select a harness that defines it with --harness"
            ))
        })
    }

    pub fn resolve_model<'a>(
        &'a self,
        agent: &'a AgentDef,
        harness: &str,
        explicit: Option<&'a str>,
    ) -> Option<&'a str> {
        explicit
            .or_else(|| agent.harness.model_for(harness))
            .or(self.model.as_deref())
    }

    pub fn sends_briefing(&self) -> bool {
        match self.briefing.as_deref() {
            Some("send") => true,
            Some(_) => false,
            None => !self
                .args_json
                .iter()
                .chain(&self.args)
                .any(|a| uses_briefing(a)),
        }
    }

    fn strings(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.command.as_str())
            .chain(self.args_json.as_deref())
            .chain(self.args.iter().map(String::as_str))
            .chain(self.model_args.iter().map(String::as_str))
            .chain(self.env.values().map(String::as_str))
            .chain(
                self.files
                    .iter()
                    .flat_map(|(path, content)| [path.as_str(), content.as_str()]),
            )
            .chain(self.builtin_tools.values().flatten().map(String::as_str))
    }

    pub(crate) fn legacy_placeholders(&self) -> Result<std::collections::BTreeSet<String>> {
        let renderer = Renderer::new();
        let mut names = std::collections::BTreeSet::new();
        for source in self.strings() {
            names.extend(renderer.validate(source)?);
        }
        Ok(names)
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
    pub model: Option<String>,
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
    /// One-run model override; otherwise use the agent or profile model.
    pub model: Option<&'a str>,
    pub session_dir: &'a str,
    /// Absolute path of the trk binary the MCP server is started with.
    pub trk_bin: &'a str,
    /// TRK_DIR for the MCP server, so it reaches the project's database and config.
    pub trk_env: BTreeMap<String, String>,
    pub briefing: String,
    pub instructions: String,
    /// Names of the agent's trk tools.
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

#[derive(Serialize)]
struct SessionServer<'a> {
    name: &'a str,
    command: &'a str,
    args: &'a [String],
    env: Cow<'a, BTreeMap<String, String>>,
}

pub fn build(input: LaunchInput) -> Result<LaunchSpec> {
    let builtin_args = input
        .profile
        .builtin_args(&input.agent.harness.builtin_tools, input.harness)?;
    let model = input
        .profile
        .resolve_model(input.agent, input.harness, input.model);
    let trk_args = trk_mcp_args(input.issue, input.actor, &input.agent.name);
    let mut session_servers = Vec::with_capacity(1 + input.agent.mcp.len());
    session_servers.push(SessionServer {
        name: "trk",
        command: input.trk_bin,
        args: &trk_args,
        env: Cow::Borrowed(&input.trk_env),
    });
    for server in &input.agent.mcp {
        let env: BTreeMap<_, _> = server
            .env
            .iter()
            .map(|(key, value)| (key.clone(), expand_env(value)))
            .collect();
        session_servers.push(SessionServer {
            name: &server.name,
            command: &server.command,
            args: &server.args,
            env: Cow::Owned(env),
        });
    }

    // The launch spec and default MCP file retain their existing generic shape.
    let mut servers = serde_json::Map::new();
    for server in &session_servers {
        servers.insert(
            server.name.to_string(),
            json!({
                "command": server.command, "args": server.args, "env": server.env,
            }),
        );
    }
    let mcp_servers = json!({"mcpServers": servers});
    let session_dir = input.session_dir.to_string();
    let context = minijinja::context! {
        name => input.terminal,
        issue => input.issue,
        model => model.unwrap_or_default(),
        actor => input.actor,
        agent => input.agent.name.as_str(),
        harness => input.harness,
        cwd => input.cwd,
        worktree => input.worktree.unwrap_or(input.cwd),
        session_dir => input.session_dir,
        briefing => input.briefing.as_str(),
        briefing_file => format!("{session_dir}/briefing.md"),
        instructions => input.instructions.as_str(),
        instructions_file => format!("{session_dir}/instructions.md"),
        mcp_json_file => format!("{session_dir}/mcp.json"),
        trk_command => input.trk_bin,
        trk_args => &trk_args,
        trk_env => &input.trk_env,
        trk_tools => &input.tools,
        mcp_servers => minijinja::value::Serde(&session_servers),
    };
    let renderer = Renderer::new();
    let render = |source: &str| renderer.render(source, &context);
    let agent_args = input.agent.harness.args.get(input.harness);
    let model_args = model.map(|_| &input.profile.model_args);
    let arg_groups = [
        ("{builtin_tools_args}", builtin_args),
        ("{model_args}", model_args),
        ("{agent_args}", agent_args),
    ];
    let fallback_groups = arg_groups.map(|(marker, args)| {
        if input.profile.args.iter().any(|source| source == marker) {
            None
        } else {
            args
        }
    });
    let prefix_args: Vec<String> = input
        .profile
        .args_json
        .as_deref()
        .map(|source| {
            serde_json::from_str(&render(source)?).map_err(|e| {
                Error::new(
                    Code::ConfigInvalid,
                    format!("harness args_json must render a JSON array of strings: {e}"),
                )
            })
        })
        .transpose()?
        .unwrap_or_default();
    let mut argv = Vec::with_capacity(
        1 + prefix_args.len()
            + input.profile.args.len()
            + builtin_args.map_or(0, Vec::len)
            + model_args.map_or(0, Vec::len)
            + agent_args.map_or(0, Vec::len),
    );
    argv.push(render(&input.profile.command)?);
    argv.extend(prefix_args);
    let pos = input
        .profile
        .args
        .iter()
        .position(|source| uses_briefing(source))
        .unwrap_or(input.profile.args.len());
    for index in 0..=input.profile.args.len() {
        // Each unmarked group retains its legacy position and ordering.
        if index == pos {
            for source in fallback_groups.into_iter().flatten().flatten() {
                argv.push(render(source)?);
            }
        }
        if let Some(source) = input.profile.args.get(index) {
            let sources = match arg_groups
                .iter()
                .find(|(marker, _)| source.as_str() == *marker)
            {
                Some((_, args)) => args.map(Vec::as_slice).unwrap_or_default(),
                None => std::slice::from_ref(source),
            };
            for source in sources {
                argv.push(render(source)?);
            }
        }
    }

    let mut env = BTreeMap::from([("TRK_ACTOR".into(), input.actor.to_string())]);
    for (key, source) in &input.profile.env {
        env.insert(key.clone(), render(source)?);
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
    for (path, content) in &input.profile.files {
        let path = render(path)?;
        let abs = if Path::new(&path).is_absolute() {
            path
        } else {
            format!("{}/{}", input.cwd, path)
        };
        files.insert(abs, render(content)?);
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
        model: model.map(String::from),
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
