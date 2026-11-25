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
        "types/change.toml",
        "description = 'Change code'\nagent = 'custom reviewer'\nworktree = false\n",
    );
    put(
        &cfg_dir,
        "agents/custom reviewer.toml",
        "name = 'custom reviewer'\ninstructions = 'prompts/rules.j2'\n",
    );
    put(
        &cfg_dir,
        "agents/other.toml",
        "name = 'other'\ninstructions = 'prompts/rules.j2'\n",
    );
    put(
        &cfg_dir,
        "prompts/rules.j2",
        "{% include 'prompts/fragment.j2' %}: {{ issue.type }} / {{ type.name }} ({{ type.description }}) / {{ agent.name }} targeting {{ base }}",
    );
    put(&cfg_dir, "prompts/fragment.j2", "Project policy");
    let cfg = RoutingConfig::new(&ConfigDir::with_user_dir(&cfg_dir, None));
    let route = cfg.resolve("change", None).unwrap();
    assert_eq!(route.description, "Change code");
    assert_eq!(route.agent, "custom reviewer");
    assert!(!route.worktree);
    assert_eq!(route.type_source, cfg_dir.join("types/change.toml"));
    assert_eq!(
        route.agent_source,
        cfg_dir.join("agents/custom reviewer.toml")
    );
    let resolved = cfg.load(&route).unwrap();
    let issue = trk_agent::check::sample_issue(&trk_core::config::UserConfig::default());
    let input = trk_agent::prompt::PromptInput {
        issue: &issue,
        epics: &[],
        project_root: dir.path(),
        config: &trk_core::config::UserConfig::default(),
        issue_type: &route.issue_type,
        type_description: &route.description,
        type_guidance: route.guidance.as_deref(),
        agent: &resolved.agent,
        tools: &[],
        worktree: None,
        base: "release/stable",
        verify: "",
        actor: "human:test@example.com",
        harness: None,
    };
    assert_eq!(
        trk_agent::prompt::instructions(&resolved.config, &input).unwrap(),
        "Project policy: change / change (Change code) / custom reviewer targeting release/stable"
    );

    // An explicit agent overrides the type's mapping.
    let route = cfg.resolve("change", Some("other")).unwrap();
    assert!(route.override_agent);
    assert!(!route.worktree);
    let resolved = cfg.load(&route).unwrap();
    assert_eq!(resolved.agent.name, "other");
    let input = trk_agent::prompt::PromptInput {
        agent: &resolved.agent,
        issue_type: &route.issue_type,
        type_description: &route.description,
        type_guidance: route.guidance.as_deref(),
        ..input
    };
    assert_eq!(
        trk_agent::prompt::instructions(&resolved.config, &input).unwrap(),
        "Project policy: change / change (Change code) / other targeting release/stable"
    );
}

#[test]
fn missing_types_and_agents_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = dir.path().join(".trk");
    put(
        &cfg_dir,
        "types/change.toml",
        "description = 'Change code'\nagent = 'implementer'\n",
    );
    put(&cfg_dir, "types/empty.toml", "description = 'No agent'\n");
    let cfg = RoutingConfig::new(&ConfigDir::with_user_dir(&cfg_dir, None));
    assert_eq!(
        cfg.resolve("nope", None).unwrap_err().code(),
        trk_core::Code::NotFound
    );
    assert_eq!(
        cfg.resolve("change", None).unwrap_err().code(),
        trk_core::Code::NotFound
    );
    assert_eq!(
        cfg.resolve("empty", None).unwrap_err().code(),
        trk_core::Code::ConfigInvalid
    );
    assert!(cfg.resolve("../escape", None).is_err());
}
