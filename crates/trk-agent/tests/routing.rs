use trk_agent::routing::RoutingConfig;
use trk_core::config::ConfigDir;

fn put(root: &std::path::Path, name: &str, text: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn a_type_selects_its_agent_and_prompts_from_the_project_config() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = dir.path().join(".trk");
    put(
        &cfg_dir,
        "types/code-change.toml",
        "description = 'Change code'\nagent = 'custom reviewer'\nworktree = false\n",
    );
    put(
        &cfg_dir,
        "agents/custom reviewer.toml",
        "name = 'custom reviewer'\ninstructions = 'prompts/rules.j2'\n",
    );
    put(&cfg_dir, "agents/other.toml", "name = 'other'\n");
    put(
        &cfg_dir,
        "prompts/rules.j2",
        "{% include 'prompts/fragment.j2' %}: {{ issue.type }} targeting {{ base }}",
    );
    put(&cfg_dir, "prompts/fragment.j2", "Project policy");
    let cfg = RoutingConfig::new(&ConfigDir::new(&cfg_dir));
    let route = cfg.resolve("code-change", None).unwrap();
    assert_eq!(route.description, "Change code");
    assert_eq!(route.agent, "custom reviewer");
    assert!(!route.worktree);
    assert_eq!(route.type_source, cfg_dir.join("types/code-change.toml"));
    assert_eq!(
        route.agent_source,
        cfg_dir.join("agents/custom reviewer.toml")
    );
    let resolved = cfg.load(&route).unwrap();
    let issue = trk_agent::check::sample_issue();
    let input = trk_agent::prompt::PromptInput {
        issue: &issue,
        agent: &resolved.agent,
        tools: &[],
        worktree: None,
        base: "release/stable",
        actor: "human:test@example.com",
        harness: None,
    };
    assert_eq!(
        trk_agent::prompt::instructions(&resolved.config, &input).unwrap(),
        "Project policy: code-change targeting release/stable"
    );

    // An explicit agent overrides the type's mapping.
    let route = cfg.resolve("code-change", Some("other")).unwrap();
    assert!(route.override_agent);
    assert!(!route.worktree);
    assert_eq!(cfg.load(&route).unwrap().agent.name, "other");
}

#[test]
fn missing_types_and_agents_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = dir.path().join(".trk");
    put(
        &cfg_dir,
        "types/code-change.toml",
        "description = 'Change code'\nagent = 'implementer'\n",
    );
    put(&cfg_dir, "types/empty.toml", "description = 'No agent'\n");
    let cfg = RoutingConfig::new(&ConfigDir::new(&cfg_dir));
    assert_eq!(
        cfg.resolve("nope", None).unwrap_err().code(),
        trk_core::Code::NotFound
    );
    assert_eq!(
        cfg.resolve("code-change", None).unwrap_err().code(),
        trk_core::Code::NotFound
    );
    assert_eq!(
        cfg.resolve("empty", None).unwrap_err().code(),
        trk_core::Code::ConfigInvalid
    );
    assert!(cfg.resolve("../escape", None).is_err());
}
