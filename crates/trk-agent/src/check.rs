//! `trk agent check`: diagnostics for agent definitions, their templates and
//! the harness profiles they prefer.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use trk_core::config::{ConfigDir, Diagnostic, UserConfig};

use crate::agent::{AgentDef, apply_preferences, load_agent_file};
use crate::harness::{installed, profiles};
use crate::prompt::{PromptInput, briefing, instructions};
use crate::tools::{agent_tools, unmatched_includes};

/// A sample issue object for rendering templates during checks.
pub fn sample_issue(user: &UserConfig) -> Value {
    json!({
        "id": "proj-abc", "title": "Sample",
        "description": "Sample description", "state": "claimed", "priority": "P2",
        "type": user.issues.default_type, "close_reason": null, "waiting": null, "claim": null,
        "deps": {"blocked_by": [], "blocks": []},
        "derived": {"ready": false, "blocked_by": [], "due": false, "unblocks_higher_priority": []},
        "notes": [],
    })
}

pub fn check_agents(cfg: &ConfigDir, user: &UserConfig) -> Vec<Diagnostic> {
    let mut d = vec![];
    let default_type = &user.issues.default_type;
    if !cfg.files_in("types", "toml").iter().any(|path| {
        path.is_file()
            && path
                .file_stem()
                .is_some_and(|name| name == default_type.as_str())
    }) {
        d.push(Diagnostic::error(
            Some("config.toml"),
            format!(
                "issues.default_type '{default_type}': .trk/types/{default_type}.toml does not exist"
            ),
        ));
    }
    let landing = format!("prompts/_landing.{}.md.j2", user.run.landing);
    if !cfg.resolve(&landing).is_file() {
        d.push(Diagnostic::error(
            Some("config.toml"),
            format!(
                "run.landing '{}': template '{landing}' does not exist",
                user.run.landing
            ),
        ));
    }
    let mut agents: BTreeMap<String, AgentDef> = BTreeMap::new();
    for p in cfg.files_in("agents", "toml") {
        let file = format!("agents/{}", p.file_name().unwrap().to_string_lossy());
        match load_agent_file(&p) {
            Ok(mut a) => {
                if let Err(e) = apply_preferences(&mut a, user) {
                    d.push(Diagnostic::error(Some("config.toml"), e.message()));
                }
                let stem = p.file_stem().unwrap().to_string_lossy().to_string();
                if stem != a.name {
                    d.push(Diagnostic::warning(
                        Some(&file),
                        format!("agent name '{}' differs from its file name", a.name),
                    ));
                }
                agents.insert(a.name.clone(), a);
            }
            Err(e) => d.push(Diagnostic::error(Some(&file), e.message())),
        }
    }
    let routing = crate::routing::RoutingConfig::new(cfg);
    if let Err(e) = routing.types() {
        d.push(Diagnostic::error(Some("types"), e.message()));
    }
    let profs = match profiles(user) {
        Ok(p) => p,
        Err(e) => {
            d.push(Diagnostic::error(Some("config.toml"), e.message()));
            BTreeMap::new()
        }
    };
    for (name, profile) in &profs {
        if let Err(error) = profile.validate_templates() {
            d.push(Diagnostic::error(
                Some("config.toml"),
                format!("[harness.{name}]: {error}"),
            ));
        }
    }
    let levels: BTreeSet<&str> = std::iter::once("default")
        .chain(
            profs
                .values()
                .flat_map(|p| p.builtin_tools.keys().map(String::as_str)),
        )
        .collect();
    for a in agents.values() {
        let file = format!("agents/{}.toml", a.name);
        let file = file.as_str();
        if !levels.contains(a.harness.builtin_tools.as_str()) {
            d.push(Diagnostic::error(
                Some(file),
                format!(
                    "builtin_tools '{}' is not defined by any harness profile; available levels: {}",
                    a.harness.builtin_tools,
                    levels.iter().copied().collect::<Vec<_>>().join(", ")
                ),
            ));
        }
        for h in a.harness.prefer.as_deref().unwrap_or_default() {
            match profs.get(h) {
                None => d.push(Diagnostic::warning(
                    Some(file),
                    format!("preferred harness '{h}' has no [harness.{h}] profile"),
                )),
                Some(p) => {
                    let lvl = a.harness.builtin_tools.as_str();
                    if lvl != "default" && !p.builtin_tools.contains_key(lvl) {
                        d.push(Diagnostic::warning(
                            Some(file),
                            format!("harness '{h}' cannot enforce builtin_tools = \"{lvl}\" (no builtin_tools.{lvl} in its profile)"),
                        ));
                    }
                    if !installed(&p.command) {
                        d.push(Diagnostic::warning(
                            Some(file),
                            format!("harness '{h}' command '{}' is not on PATH", p.command),
                        ));
                    }
                }
            }
        }
        for name in [&a.instructions, &a.briefing].into_iter().flatten() {
            if !cfg.resolve(name).exists() {
                d.push(Diagnostic::error(
                    Some(file),
                    format!("template '{name}' does not exist"),
                ));
            }
        }
        for miss in unmatched_includes(a, user) {
            d.push(Diagnostic::error(
                Some(file),
                format!("trk_tools.include: there is no tool '{miss}'"),
            ));
        }
        let tools = agent_tools(a, user);
        let ends = |n: &str| tools.iter().any(|t| t.name == n);
        if !ends("close") && !ends("wait") && !ends("hand_back") {
            d.push(Diagnostic::error(
                Some(file),
                "the agent has none of close, wait, hand_back: it could never end a session",
            ));
        }
        let issue = sample_issue(user);
        let input = PromptInput {
            issue: &issue,
            project_root: std::path::Path::new("/tmp/project"),
            config: user,
            issue_type: &user.issues.default_type,
            type_description: "Sample type description",
            agent: a,
            tools: &tools,
            worktree: Some("/tmp/wt"),
            base: user.run.base.as_deref().unwrap_or("main"),
            verify: &user.check.verify,
            actor: "agent:x:1",
            harness: None,
        };
        match instructions(cfg, &input) {
            Ok(i) => {
                if let Err(e) = briefing(cfg, &input, Some(&i)) {
                    d.push(Diagnostic::error(Some(file), e.message()));
                }
            }
            Err(e) => d.push(Diagnostic::error(Some(file), e.message())),
        }
    }
    d
}
