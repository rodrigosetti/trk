use std::path::Path;

use serde_json::json;
use trk_agent::prompt::{self, PromptInput};
use trk_agent::routing::RoutingConfig;
use trk_core::config::{ConfigDir, UserConfig};

fn put(root: &Path, name: &str, text: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn guidance_keeps_type_context_and_layered_includes_with_agent_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let user = dir.path().join("user");
    put(
        &user,
        "types/bug.toml",
        "description = 'User bug'\nagent = 'mapped'\nguidance = 'User guidance'\n",
    );
    put(
        &project,
        "types/bug.toml",
        r#"description = 'Fix a bug'
agent = 'mapped'
guidance = '''{{ type.name }} / {{ type.description }} / {{ issue.title }} / {{ agent.name }} / {{ base }} / {{ verify }} / {{ actor }} / {{ harness }} / {{ worktree }} / {{ config.check.interval }} / {{ tools[0].name }}: {% include 'prompts/reproduce.j2' %}'''
"#,
    );
    for name in ["mapped", "override"] {
        put(
            &user,
            &format!("agents/{name}.toml"),
            &format!("name = '{name}'\ninstructions = 'prompts/instructions.j2'\n"),
        );
    }
    put(&user, "prompts/instructions.j2", "Agent {{ agent.name }}");
    put(
        &user,
        "prompts/reproduce.j2",
        "Reproduce {{ issue.type }}; {% include 'prompts/regression.j2' %}",
    );
    put(&user, "prompts/regression.j2", "user regression rule");
    put(&project, "prompts/regression.j2", "land a regression test");
    let cfg = ConfigDir::with_user_dir(&project, Some(user.clone()));
    let routing = RoutingConfig::new(&cfg);
    let config = UserConfig::default();
    let issue = json!({"id": "proj-abc", "type": "bug", "title": "Literal {{ agent.name }}"});
    for override_agent in [None, Some("override")] {
        let route = routing.resolve("bug", override_agent).unwrap();
        let resolved = routing.load(&route).unwrap();
        let tools = trk_agent::agent_tools(&resolved.agent, &config);
        let input = PromptInput {
            issue: &issue,
            epics: &[],
            project_root: dir.path(),
            config: &config,
            issue_type: &route.issue_type,
            type_description: &route.description,
            type_guidance: route.guidance.as_deref(),
            agent: &resolved.agent,
            tools: &tools,
            worktree: Some("/tmp/bug-worktree"),
            base: "develop",
            verify: "cargo test",
            actor: "agent:local:1",
            harness: Some("local"),
        };
        let expected_guidance = format!(
            "bug / Fix a bug / Literal {{{{ agent.name }}}} / {} / develop / cargo test / agent:local:1 / local / /tmp/bug-worktree / {} / {}: Reproduce bug; land a regression test",
            route.agent, config.check.interval, tools[0].name,
        );
        let expected_instructions = format!(
            "Agent {}\n\n## Type guidance\n\n{expected_guidance}",
            route.agent,
        );
        for fold in [false, true] {
            let rendered = prompt::render_prompts(&cfg, &input, fold).unwrap();
            assert_eq!(
                rendered.guidance.as_deref(),
                Some(expected_guidance.as_str())
            );
            assert_eq!(rendered.instructions, expected_instructions);
            assert_eq!(rendered.briefing.contains(&expected_instructions), fold);
            for (name, root, layer) in [
                ("types/bug.toml#guidance", &project, "project"),
                ("prompts/reproduce.j2", &user, "user"),
                ("prompts/regression.j2", &project, "project"),
            ] {
                assert_eq!(rendered.sources[name].layer, layer);
                assert_eq!(
                    rendered.sources[name].source,
                    root.join(name.trim_end_matches("#guidance")),
                );
            }
        }
        assert_eq!(
            prompt::instructions(&cfg, &input).unwrap(),
            expected_instructions
        );
    }
}

#[test]
fn absent_and_empty_guidance_preserve_instructions_including_the_default() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), "agents/plain.toml", "name = 'plain'\n");
    put(
        dir.path(),
        "types/task.toml",
        "description = 'Task'\nagent = 'plain'\n",
    );
    let cfg = ConfigDir::with_user_dir(dir.path(), None);
    let routing = RoutingConfig::new(&cfg);
    let route = routing.resolve("task", None).unwrap();
    let resolved = routing.load(&route).unwrap();
    let config = UserConfig::default();
    let without = prompt::type_prompts(&cfg, &resolved.agent, &config, &route).unwrap();
    assert!(without.guidance.is_none());
    assert!(!without.sources.contains_key("types/task.toml#guidance"));
    for guidance in ["", " \n ", "{% if false %}ignored{% endif %}"] {
        let mut empty = route.clone();
        empty.guidance = Some(guidance.into());
        let rendered = prompt::type_prompts(&cfg, &resolved.agent, &config, &empty).unwrap();
        assert_eq!(rendered.instructions, without.instructions);
        assert_eq!(rendered.briefing, without.briefing);
    }
    let mut guided = route;
    guided.guidance = Some("Always reproduce first.".into());
    let rendered = prompt::type_prompts(&cfg, &resolved.agent, &config, &guided).unwrap();
    assert_eq!(
        rendered.instructions,
        format!(
            "{}\n\n## Type guidance\n\nAlways reproduce first.",
            without.instructions
        ),
    );
}
