use trk_agent::routing::RoutingConfig;
use trk_core::config::ConfigDir;

fn put(root: &std::path::Path, name: &str, text: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn global_prompts_cannot_be_shadowed_by_repository_prompts() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("global");
    let local = dir.path().join("repo/.trk");
    put(
        &global,
        "types/code-change.toml",
        "description = 'Change code'\nagent = 'custom reviewer'\n",
    );
    put(
        &global,
        "agents/custom reviewer.toml",
        "name = 'custom reviewer'\ninstructions = 'prompts/rules.j2'\n",
    );
    put(
        &global,
        "prompts/rules.j2",
        "{% include 'prompts/fragment.j2' %}: {{ issue.type }}",
    );
    put(&global, "prompts/fragment.j2", "Global policy");
    put(
        &local,
        "types/code-change.toml",
        "agent = 'custom reviewer'\n",
    );
    put(&local, "prompts/rules.j2", "Repository policy");
    put(&local, "prompts/fragment.j2", "Repository fragment");
    let cfg = RoutingConfig {
        global: ConfigDir::new(&global),
        repo: Some(ConfigDir::new(&local)),
    };
    let route = cfg.resolve("code-change", None).unwrap();
    assert_eq!(route.description, "Change code");
    assert_eq!(
        route.description_source,
        global.join("types/code-change.toml")
    );
    assert_eq!(route.mapping_source, local.join("types/code-change.toml"));
    assert!(!route.requires_trust);
    let resolved = cfg.load(&route).unwrap();
    let issue = trk_agent::check::sample_issue();
    let input = trk_agent::prompt::PromptInput {
        issue: &issue,
        agent: &resolved.agent,
        tools: &[],
        worktree: None,
        actor: "human:test@example.com",
        harness: None,
    };
    assert_eq!(
        trk_agent::prompt::instructions(&resolved.config, &input).unwrap(),
        "Global policy: code-change"
    );
}

#[test]
fn migration_can_still_launch_an_existing_unicode_agent_name() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("global");
    put(&global, "agents/réviseur.toml", "name = 'réviseur'\n");
    let db = dir.path().join("old.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(include_str!("../../trk-core/src/schema.sql"))
        .unwrap();
    conn.execute_batch(
        "PRAGMA user_version = 1;
         INSERT INTO project VALUES ('p', 'Project', '2026-11-20T10:00:00Z');
         INSERT INTO actor (id, kind, first_seen)
         VALUES ('human:test@example.com', 'human', '2026-11-20T10:00:00Z');
         INSERT INTO issue (id, project_id, title, agent, created_by, created_at, updated_at)
         VALUES ('p-abc', 'p', 'Review', 'réviseur', 'human:test@example.com',
                 '2026-11-20T10:00:00Z', '2026-11-20T10:00:00Z');",
    )
    .unwrap();
    drop(conn);
    let store = trk_core::Store::open(
        &db,
        trk_core::StoreOpts {
            migration_config: Some(global.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let issue = store
        .read(None, |c| c.show("p-abc", Default::default()))
        .unwrap();
    let cfg = RoutingConfig::for_issue(&ConfigDir::new(global), &issue).unwrap();
    let route = cfg.resolve(issue["type"].as_str().unwrap(), None).unwrap();
    assert_eq!(cfg.load(&route).unwrap().agent.name, "réviseur");
}

#[test]
fn validation_checks_replacement_agent_not_obsolete_global_templates() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("global");
    let local = dir.path().join("repo/.trk");
    put(
        &global,
        "types/code-change.toml",
        "description = 'Change code'\nagent = 'implementer'\n",
    );
    put(
        &global,
        "agents/implementer.toml",
        "name = 'implementer'\ninstructions = 'prompts/not-installed.j2'\n",
    );
    put(&local, "agents/implementer.toml", "name = 'implementer'\n");
    let cfg = RoutingConfig {
        global: ConfigDir::new(global),
        repo: Some(ConfigDir::new(local)),
    };
    let diagnostics = cfg.check(&Default::default());
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.level == trk_core::config::Level::Error),
        "{diagnostics:?}"
    );
    let route = cfg.resolve("code-change", None).unwrap();
    let replacement = cfg.load(&route).unwrap();
    assert!(replacement.agent.instructions.is_none());
    assert!(route.requires_trust);
}
