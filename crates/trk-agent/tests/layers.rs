use std::path::Path;

use serde_json::json;
use trk_agent::harness;
use trk_agent::prompt::{self, PromptInput};
use trk_agent::routing::RoutingConfig;
use trk_agent::{load_agent, load_agents};
use trk_core::config::{ConfigDir, Level, UserConfig};
use trk_core::error::Code;

fn put(root: &Path, name: &str, text: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn project_files_override_user_files_and_both_layers_are_enumerated() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let user = dir.path().join("user");
    put(
        &user,
        "types/change.toml",
        "description = 'User type'\nagent = 'personal'\n",
    );
    put(
        &project,
        "types/change.toml",
        "description = 'Project type'\nagent = 'shared'\n",
    );
    put(
        &user,
        "types/personal.toml",
        "description = 'Personal type'\nagent = 'personal'\n",
    );
    put(
        &user,
        "agents/shared.toml",
        "name = 'shared'\ndescription = 'User agent'\nbriefing = 'ignored.j2'\n",
    );
    put(
        &project,
        "agents/shared.toml",
        "name = 'shared'\ninstructions = 'prompts/rules.j2'\n",
    );
    put(
        &user,
        "agents/personal.toml",
        "name = 'personal'\ninstructions = 'prompts/rules.j2'\n",
    );
    put(&user, "prompts/rules.j2", "user prompt");
    put(&project, "prompts/rules.j2", "project prompt");
    let cfg = ConfigDir::with_user_dir(&project, Some(user.clone()));
    let routing = RoutingConfig::new(&cfg);
    let route = routing.resolve("change", None).unwrap();
    assert_eq!(route.description, "Project type");
    assert_eq!(route.agent, "shared");
    assert_eq!(route.type_layer, "project");
    assert_eq!(route.agent_layer, "project");
    assert_eq!(route.type_source, project.join("types/change.toml"));
    assert_eq!(route.agent_source, project.join("agents/shared.toml"));
    let resolved = routing.load(&route).unwrap();
    assert!(resolved.agent.description.is_none());
    assert!(resolved.agent.briefing.is_none());
    assert_eq!(
        prompt::render_template(&cfg, "prompts/rules.j2", &json!({})).unwrap(),
        "project prompt"
    );
    let personal = routing.resolve("personal", None).unwrap();
    assert_eq!(personal.type_layer, "user");
    assert_eq!(personal.agent_layer, "user");
    assert_eq!(personal.type_source, user.join("types/personal.toml"));
    assert_eq!(personal.agent_source, user.join("agents/personal.toml"));
    let override_route = routing.resolve("change", Some("personal")).unwrap();
    assert_eq!(override_route.type_layer, "project");
    assert_eq!(override_route.agent_layer, "user");
    assert_eq!(
        routing
            .agents()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["personal", "shared"]
    );
    assert_eq!(
        routing
            .types()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["change", "personal"]
    );
    assert_eq!(load_agents(&cfg).unwrap().len(), 2);
    assert!(load_agent(&cfg, "shared").unwrap().description.is_none());
    assert_eq!(load_agent(&cfg, "personal").unwrap().name, "personal");

    // Loading one agent must not read an unrelated broken file.
    put(&project, "agents/broken.toml", "not valid toml");
    assert_eq!(load_agent(&cfg, "personal").unwrap().name, "personal");
}

#[test]
fn templates_cross_layers_for_includes_imports_and_inheritance_and_report_sources() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let user = dir.path().join("user");
    put(
        &user,
        "prompts/_landing.merge.md.j2",
        include_str!("../../../seed/prompts/_landing.merge.md.j2"),
    );
    put(
        &user,
        "agents/personal.toml",
        "name = 'personal'\ninstructions = 'prompts/instructions.j2'\nbriefing = 'prompts/briefing.j2'\n",
    );
    put(
        &user,
        "types/change.toml",
        "description = 'Change code'\nagent = 'personal'\n",
    );
    put(
        &user,
        "prompts/instructions.j2",
        "{% extends 'prompts/layout.j2' %}{% block content %}root/{% include 'prompts/project.j2' %}/{% import 'prompts/macros.j2' as m %}{{ m.rule() }}{% endblock %}",
    );
    put(
        &user,
        "prompts/layout.j2",
        "[{% block content %}{% endblock %}]",
    );
    put(
        &user,
        "prompts/macros.j2",
        "{% macro rule() %}macro{% endmacro %}",
    );
    put(
        &project,
        "prompts/project.j2",
        "project/{% include 'prompts/user.j2' %}",
    );
    put(&user, "prompts/user.j2", "user");
    put(
        &project,
        "prompts/briefing.j2",
        "{% include 'prompts/user.j2' %}|{{ instructions }}",
    );
    let cfg = ConfigDir::with_user_dir(&project, Some(user.clone()));
    let agent = load_agent(&cfg, "personal").unwrap();
    let config = UserConfig::default();
    let issue = trk_agent::check::sample_issue(&config);
    let input = PromptInput {
        issue: &issue,
        project_root: dir.path(),
        config: &config,
        issue_type: "change",
        type_description: "Change code",
        type_guidance: None,
        agent: &agent,
        tools: &[],
        worktree: None,
        base: "main",
        verify: "",
        actor: "agent:personal:1",
        harness: None,
    };
    let instructions = prompt::instructions(&cfg, &input).unwrap();
    assert_eq!(instructions, "[root/project/user/macro]");
    assert_eq!(
        prompt::briefing(&cfg, &input, Some(&instructions)).unwrap(),
        "user|[root/project/user/macro]"
    );
    let sources = prompt::template_sources(&cfg, &input).unwrap();
    assert_eq!(sources.len(), 6);
    for name in ["instructions", "layout", "macros", "user"] {
        let name = format!("prompts/{name}.j2");
        assert_eq!(sources[&name].layer, "user");
        assert_eq!(sources[&name].source, user.join(name));
    }
    for name in ["project", "briefing"] {
        let name = format!("prompts/{name}.j2");
        assert_eq!(sources[&name].layer, "project");
        assert_eq!(sources[&name].source, project.join(name));
    }
    assert_eq!(
        prompt::agent_template_sources(&cfg, &agent, &config)
            .unwrap()
            .len(),
        6
    );
    assert!(
        !trk_agent::check::check_agents(&cfg, &config)
            .iter()
            .any(|d| d.level == Level::Error)
    );
}

#[test]
fn global_harness_preferences_are_inherited_but_explicit_lists_replace_them() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let user = dir.path().join("user");
    put(
        &user,
        "prompts/_landing.pr.md.j2",
        include_str!("../../../seed/prompts/_landing.pr.md.j2"),
    );
    put(&project, "config.toml", "");
    put(
        &user,
        "config.toml",
        "[issues]\ndefault_type = 'change'\n[run]\nlanding = 'pr'\n[harness]\nprefer = ['z']\n[harness.a]\ncommand = '/bin/sh'\n[harness.z]\ncommand = '/bin/sh'\n",
    );
    put(&user, "agents/inherited.toml", "name = 'inherited'\n");
    put(
        &user,
        "agents/explicit.toml",
        "name = 'explicit'\n[harness]\nprefer = ['a']\n",
    );
    put(
        &user,
        "agents/empty.toml",
        "name = 'empty'\n[harness]\nprefer = []\n",
    );
    put(
        &user,
        "types/change.toml",
        "description = 'Change'\nagent = 'inherited'\n",
    );
    let cfg = ConfigDir::with_user_dir(&project, Some(user));
    let config = cfg.user_config().unwrap();
    let profiles = harness::profiles(&config).unwrap();
    assert_eq!(
        profiles.keys().map(String::as_str).collect::<Vec<_>>(),
        ["a", "z"]
    );
    let agents = load_agents(&cfg).unwrap();
    assert_eq!(
        agents["inherited"].harness.prefer.as_deref().unwrap(),
        ["z"]
    );
    assert_eq!(agents["explicit"].harness.prefer.as_deref().unwrap(), ["a"]);
    assert!(
        agents["empty"]
            .harness
            .prefer
            .as_deref()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        harness::choose(&agents["inherited"], &profiles, None).unwrap(),
        "z"
    );
    assert_eq!(
        harness::choose(&agents["explicit"], &profiles, None).unwrap(),
        "a"
    );
    assert_eq!(
        harness::choose(&agents["empty"], &profiles, None).unwrap(),
        "a"
    );
    assert_eq!(
        harness::choose(&agents["inherited"], &profiles, Some("a")).unwrap(),
        "a"
    );
    let routing = RoutingConfig::new(&cfg);
    let route = routing.resolve("change", None).unwrap();
    assert_eq!(
        routing
            .load(&route)
            .unwrap()
            .agent
            .harness
            .prefer
            .as_deref()
            .unwrap(),
        ["z"]
    );
    assert_eq!(
        routing.agents().unwrap()["inherited"]
            .agent
            .harness
            .prefer
            .as_deref()
            .unwrap(),
        ["z"]
    );
    assert_eq!(
        load_agent(&cfg, "inherited")
            .unwrap()
            .harness
            .prefer
            .as_deref()
            .unwrap(),
        ["z"]
    );
    assert!(
        !trk_agent::check::check_agents(&cfg, &config)
            .iter()
            .any(|d| d.level == Level::Error)
    );

    // A project-level global preference replaces, rather than appends to, the user list.
    put(&project, "config.toml", "[harness]\nprefer = ['a']\n");
    assert_eq!(
        load_agent(&cfg, "inherited")
            .unwrap()
            .harness
            .prefer
            .as_deref()
            .unwrap(),
        ["a"]
    );
    assert!(
        load_agent(&cfg, "empty")
            .unwrap()
            .harness
            .prefer
            .as_deref()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn harness_preference_setting_is_validated_as_a_string_array_not_a_profile() {
    for invalid in ["'a'", "[1]", "{command = 'a'}"] {
        let mut config = UserConfig::default();
        let table: toml::Table = toml::from_str(&format!("prefer = {invalid}")).unwrap();
        config
            .harness
            .insert("prefer".into(), table["prefer"].clone());
        let error = harness::profiles(&config).unwrap_err();
        assert_eq!(error.code(), Code::ConfigInvalid);
        assert!(error.message().contains("[harness] prefer"));
    }
}

#[test]
fn absent_user_layer_preserves_project_only_loading() {
    let dir = tempfile::tempdir().unwrap();
    put(
        dir.path(),
        "config.toml",
        "[harness]\nprefer = ['local']\n[harness.local]\ncommand = '/bin/sh'\n",
    );
    put(
        dir.path(),
        "agents/local.toml",
        "name = 'local'\ninstructions = 'prompts/local.j2'\n",
    );
    put(
        dir.path(),
        "types/change.toml",
        "description = 'Change'\nagent = 'local'\n",
    );
    put(dir.path(), "prompts/local.j2", "local");
    let cfg = ConfigDir::with_user_dir(dir.path(), None);
    assert_eq!(load_agent(&cfg, "local").unwrap().name, "local");
    assert_eq!(
        load_agent(&cfg, "local")
            .unwrap()
            .harness
            .prefer
            .as_deref()
            .unwrap(),
        ["local"]
    );
    let routing = RoutingConfig::new(&cfg);
    let route = routing.resolve("change", None).unwrap();
    assert_eq!(route.type_layer, "project");
    assert_eq!(route.agent_layer, "project");
    assert_eq!(routing.load(&route).unwrap().agent.name, "local");
    assert_eq!(
        prompt::render_template(&cfg, "prompts/local.j2", &json!({})).unwrap(),
        "local"
    );
}

#[test]
fn provenance_matches_the_harness_instruction_folding_context() {
    let dir = tempfile::tempdir().unwrap();
    put(
        dir.path(),
        "agents/local.toml",
        "name = 'local'\ninstructions = 'prompts/instructions.j2'\nbriefing = 'prompts/briefing.j2'\n",
    );
    put(dir.path(), "prompts/instructions.j2", "rules");
    put(
        dir.path(),
        "prompts/briefing.j2",
        "{% if instructions %}{% include 'prompts/folded.j2' %}{% else %}separate{% endif %}",
    );
    put(dir.path(), "prompts/folded.j2", "folded");
    let cfg = ConfigDir::with_user_dir(dir.path(), None);
    let agent = load_agent(&cfg, "local").unwrap();
    let config = UserConfig::default();
    let issue = trk_agent::check::sample_issue(&config);
    let input = PromptInput {
        issue: &issue,
        project_root: dir.path(),
        config: &config,
        issue_type: "change",
        type_description: "Change code",
        type_guidance: None,
        agent: &agent,
        tools: &[],
        worktree: None,
        base: "main",
        verify: "",
        actor: "agent:local:1",
        harness: None,
    };
    let separate = prompt::render_prompts(&cfg, &input, false).unwrap();
    assert_eq!(separate.instructions, "rules");
    assert_eq!(separate.briefing, "separate");
    assert!(!separate.sources.contains_key("prompts/folded.j2"));
    let folded = prompt::render_prompts(&cfg, &input, true).unwrap();
    assert_eq!(folded.briefing, "folded");
    assert_eq!(
        folded.sources["prompts/folded.j2"].source,
        dir.path().join("prompts/folded.j2")
    );
}

#[test]
fn template_inspection_uses_the_inherited_default_issue_type() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let user = dir.path().join("user");
    put(&project, "config.toml", "[issues]\nprefix = 'project'\n");
    put(
        &user,
        "config.toml",
        "[issues]\ndefault_type = 'personal'\n",
    );
    put(
        &user,
        "agents/personal.toml",
        "name = 'personal'\ninstructions = 'prompts/instructions.j2'\n",
    );
    put(
        &user,
        "types/personal.toml",
        "description = 'Personal task'\nagent = 'personal'\n",
    );
    put(
        &user,
        "prompts/instructions.j2",
        "{% include 'prompts/' ~ issue.type ~ '-' ~ type.name ~ '.j2' %}",
    );
    put(&user, "prompts/personal-personal.j2", "Personal task rules");
    let cfg = ConfigDir::with_user_dir(&project, Some(user.clone()));
    let config = cfg.user_config().unwrap();
    let agent = load_agent(&cfg, "personal").unwrap();
    let sources = prompt::agent_template_sources(&cfg, &agent, &config).unwrap();
    let source = &sources["prompts/personal-personal.j2"];
    assert_eq!(source.layer, "user");
    assert_eq!(source.source, user.join("prompts/personal-personal.j2"));
}
