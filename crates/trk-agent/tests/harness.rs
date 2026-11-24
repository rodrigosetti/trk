use std::collections::BTreeMap;

use trk_agent::agent::{AgentDef, McpServer};
use trk_agent::harness::{self, HarnessProfile, LaunchInput, LaunchSpec};
use trk_core::config::{ConfigDir, Level, UserConfig};
use trk_core::error::Code;

const BRIEFING: &str = "Literal { {{ {% and {issue} {trk_tools:evil}\nDon't expand me.";

fn seed_config() -> UserConfig {
    toml::from_str(
        &trk_core::seed::CONFIG_TOML
            .replace("{{EMAIL_LINE}}", "email = \"a@b.c\"")
            .replace("{{PREFIX}}", "proj")
            .replace(
                "{{DEFAULT_TYPE}}",
                &trk_core::config::IssuesSection::default().default_type,
            )
            .replace("{{BASE}}", "\"main\""),
    )
    .unwrap()
}

fn agent(level: &str) -> AgentDef {
    let mut agent: AgentDef = toml::from_str("name = 'reviewer'").unwrap();
    agent.harness.builtin_tools = level.into();
    agent
}

fn launch(
    profile: &HarnessProfile,
    name: &str,
    agent: &AgentDef,
) -> trk_core::error::Result<LaunchSpec> {
    harness::build(LaunchInput {
        issue: "proj-abc",
        agent,
        harness: name,
        profile,
        model: None,
        actor: "agent:reviewer:session",
        on_behalf_of: Some("human:a@b.c"),
        terminal: "trk-proj-abc",
        cwd: "/work/proj-abc",
        worktree: None,
        session_dir: "/state/session",
        trk_bin: "/bin/trk",
        trk_env: BTreeMap::from([("TRK_DIR".into(), "/project/.trk".into())]),
        briefing: BRIEFING.into(),
        instructions: "rules <&> \"quoted\"".into(),
        tools: vec!["get_task".into(), "add_note".into()],
    })
}

#[test]
fn seed_profiles_preserve_complete_legacy_launch_specs() {
    let current = harness::profiles(&seed_config()).unwrap();
    let fixture: toml::Table =
        toml::from_str(include_str!("fixtures/harness-legacy.toml")).unwrap();
    let mut legacy = seed_config();
    legacy.harness = fixture["harness"].clone().try_into().unwrap();
    let legacy = harness::profiles(&legacy).unwrap();
    assert_eq!(current.len(), 5);
    assert_eq!(
        current.keys().collect::<Vec<_>>(),
        legacy.keys().collect::<Vec<_>>()
    );
    for (level, configured) in ["default", "read-only", "none"]
        .into_iter()
        .flat_map(|level| [false, true].map(|configured| (level, configured)))
    {
        let mut agent = agent(level);
        if configured {
            agent.harness.model = Some(trk_agent::agent::AgentModel::Shared("chosen-model".into()));
            for name in current.keys() {
                agent
                    .harness
                    .args
                    .insert(name.clone(), vec!["--extra".into(), "{{ issue }}".into()]);
            }
        }
        for (name, profile) in &current {
            let actual = launch(profile, name, &agent).unwrap();
            let expected = launch(&legacy[name], name, &agent).unwrap();
            // Compare strings too: embedded MCP JSON key order and shell quoting matter.
            assert_eq!(
                serde_json::to_string(&actual).unwrap(),
                serde_json::to_string(&expected).unwrap(),
                "{name}, {level}"
            );
            let mcp = r#"{"mcpServers":{"trk":{"command":"/bin/trk","args":["mcp","--issue","proj-abc","--actor","agent:reviewer:session","--agent","reviewer"],"env":{"TRK_DIR":"/project/.trk"}}}}"#;
            if name == "omp" {
                assert_eq!(actual.files["/work/proj-abc/.omp/mcp.json"], mcp);
            }
            if name == "pi" {
                assert_eq!(actual.files["/state/session/mcp.json"], mcp);
            }
            if name == "opencode" {
                assert_eq!(
                    actual.env["OPENCODE_CONFIG_CONTENT"],
                    r#"{"$schema":"https://opencode.ai/config.json","mcp":{"trk":{"type":"local","command":["/bin/trk","mcp","--issue","proj-abc","--actor","agent:reviewer:session","--agent","reviewer"],"environment":{"TRK_DIR":"/project/.trk"},"enabled":true}}}"#
                );
            }
            assert_eq!(actual.briefing, BRIEFING);
            assert_eq!(actual.files["/state/session/briefing.md"], BRIEFING);
            if name != "opencode" {
                assert_eq!(actual.argv.last().unwrap(), BRIEFING, "{name}");
            }
        }
    }
}

#[test]
fn builtin_argument_markers_expand_at_the_requested_position_or_fall_back() {
    let cases: &[(&[&str], &[&str], usize)] = &[
        (
            &["{builtin_tools_args}", "run", "--", "{{ briefing }}"],
            &["test", "run", "--", BRIEFING],
            1,
        ),
        (
            &["run", "--", "{builtin_tools_args}", "{{ briefing }}"],
            &["test", "run", "--", BRIEFING],
            3,
        ),
        (
            &["run", "{{ briefing }}", "--", "{builtin_tools_args}"],
            &["test", "run", BRIEFING, "--"],
            4,
        ),
        (
            &["run", "--", "{{ briefing }}"],
            &["test", "run", "--", BRIEFING],
            3,
        ),
        (&["run", "--"], &["test", "run", "--"], 3),
        (
            &["run", "{builtin_tools_args}", "--"],
            &["test", "run", "--"],
            2,
        ),
    ];
    for level in ["default", "none", "read-only"] {
        for &(args, base, position) in cases {
            let mut profile: HarnessProfile = toml::from_str("command = 'test'").unwrap();
            profile.args = args.iter().map(|s| (*s).into()).collect();
            profile.builtin_tools = BTreeMap::from([
                ("default".into(), vec!["must not be used".into()]),
                ("none".into(), vec![]),
                (
                    "read-only".into(),
                    vec!["--tools".into(), "{{ issue }} read only".into(), "".into()],
                ),
            ]);
            let mut expected = base.to_vec();
            if level == "read-only" {
                expected.splice(position..position, ["--tools", "proj-abc read only", ""]);
            }
            let spec = launch(&profile, "test", &agent(level)).unwrap();
            assert_eq!(spec.argv, expected, "{level}, {args:?}");
        }
    }
}

#[test]
fn model_and_agent_argument_markers_expand_optional_groups_in_place() {
    for marker in ["{model_args}", "{agent_args}"] {
        for enabled in [false, true] {
            for position in [0, 2, 3] {
                let mut profile: HarnessProfile = toml::from_str("command = 'test'").unwrap();
                profile.args = vec!["run".into(), "--".into(), "{{ briefing }}".into()];
                profile.args.insert(position, marker.into());
                profile.model_args = vec!["--model".into(), "{{ model }}".into()];
                let mut agent = agent("default");
                let mut expected = vec!["test", "run", "--", BRIEFING];
                if enabled {
                    let group = if marker == "{model_args}" {
                        profile.model = Some("chosen model".into());
                        ["--model", "chosen model"]
                    } else {
                        agent
                            .harness
                            .args
                            .insert("test".into(), vec!["--extra".into(), "{{ issue }}".into()]);
                        ["--extra", "proj-abc"]
                    };
                    expected.splice(position + 1..position + 1, group);
                }
                assert_eq!(
                    launch(&profile, "test", &agent).unwrap().argv,
                    expected,
                    "{marker}, enabled={enabled}, position={position}"
                );
            }
        }
    }
}

#[test]
fn marked_groups_do_not_move_unmarked_groups_or_reexpand_rendered_values() {
    let mut profile: HarnessProfile = toml::from_str("command = 'test'").unwrap();
    profile.args_json = Some(r#"["{builtin_tools_args}"]"#.into());
    profile.args = vec![
        "{agent_args}".into(),
        "run".into(),
        "{{ briefing }}".into(),
        "--".into(),
        "{builtin_tools_args}".into(),
    ];
    profile.model = Some("literal {builtin_tools_args}".into());
    profile.model_args = vec!["--model".into(), "{{ model }}".into()];
    profile.builtin_tools.insert(
        "read-only".into(),
        vec!["--tools".into(), "{{ issue }}".into()],
    );
    let mut agent = agent("read-only");
    agent
        .harness
        .args
        .insert("test".into(), vec!["--extra".into(), "{{ issue }}".into()]);
    assert_eq!(
        launch(&profile, "test", &agent).unwrap().argv,
        [
            "test",
            "{builtin_tools_args}",
            "--extra",
            "proj-abc",
            "run",
            "--model",
            "literal {builtin_tools_args}",
            BRIEFING,
            "--",
            "--tools",
            "proj-abc",
        ]
    );
}

#[test]
fn structured_context_and_filters_render_every_launch_field() {
    let mut agent = agent("read-only");
    agent.mcp.push(McpServer {
        name: "aaa-extra".into(),
        command: "extra".into(),
        args: vec!["--flag".into()],
        env: BTreeMap::from([("FROM_PATH".into(), "${env:PATH}".into())]),
    });
    let mut profile: HarnessProfile = toml::from_str("command = '{{ harness }}'").unwrap();
    profile.args = vec![
        "{{ name }}|{{ issue }}|{{ actor }}|{{ agent }}|{{ cwd }}|{{ worktree }}".into(),
        "{{ session_dir }}|{{ briefing_file }}|{{ instructions_file }}|{{ mcp_json_file }}".into(),
        "{{ trk_command }}|{{ trk_args | tojson }}|{{ trk_env | toml }}".into(),
        "{{ briefing }}".into(),
    ];
    profile
        .env
        .insert("TOOLS".into(), "{{ trk_tools | join(',') }}".into());
    profile.files.insert(
        "{{ worktree }}/{{ issue }}.json".into(),
        "{{ mcp_servers | tojson }}".into(),
    );
    profile.files.insert(
        "{{ session_dir }}/instructions-copy".into(),
        "{{ instructions | tojson }}".into(),
    );
    profile.builtin_tools.insert(
        "read-only".into(),
        vec![
            "{{ mcp_servers | map(attribute='name') | join(',') }}".into(),
            "{{ trk_args | toml }}".into(),
        ],
    );
    let spec = launch(&profile, "custom", &agent).unwrap();
    assert_eq!(spec.argv[0], "custom");
    assert_eq!(
        spec.argv[1],
        "trk-proj-abc|proj-abc|agent:reviewer:session|reviewer|/work/proj-abc|/work/proj-abc"
    );
    assert_eq!(
        spec.argv[2],
        "/state/session|/state/session/briefing.md|/state/session/instructions.md|/state/session/mcp.json"
    );
    assert_eq!(
        spec.argv[3],
        "/bin/trk|[\"mcp\",\"--issue\",\"proj-abc\",\"--actor\",\"agent:reviewer:session\",\"--agent\",\"reviewer\"]|{TRK_DIR = \"/project/.trk\"}"
    );
    assert_eq!(spec.argv[4], "trk,aaa-extra");
    assert_eq!(
        spec.argv[5],
        "[\"mcp\", \"--issue\", \"proj-abc\", \"--actor\", \"agent:reviewer:session\", \"--agent\", \"reviewer\"]"
    );
    assert_eq!(spec.argv[6], BRIEFING);
    assert_eq!(spec.env["TOOLS"], "get_task,add_note");
    let servers: serde_json::Value =
        serde_json::from_str(&spec.files["/work/proj-abc/proj-abc.json"]).unwrap();
    assert_eq!(servers[0]["name"], "trk");
    assert_eq!(servers[0]["command"], "/bin/trk");
    assert_eq!(servers[0]["env"]["TRK_DIR"], "/project/.trk");
    assert_eq!(servers[1]["name"], "aaa-extra");
    assert_eq!(servers[1]["command"], "extra");
    assert_eq!(servers[1]["args"], serde_json::json!(["--flag"]));
    assert_eq!(
        servers[1]["env"]["FROM_PATH"],
        std::env::var("PATH").unwrap()
    );
    assert_eq!(
        spec.files["/state/session/instructions-copy"],
        "\"rules <&> \\\"quoted\\\"\""
    );
}

#[test]
fn legacy_and_jinja_inserted_briefings_are_never_reexpanded() {
    for template in ["{briefing}", "{{ briefing }}"] {
        let mut profile: HarnessProfile = toml::from_str("command = 'test'").unwrap();
        profile.args = vec![template.into()];
        profile.env.insert("BRIEFING".into(), template.into());
        profile.files.insert("copy".into(), template.into());
        profile
            .builtin_tools
            .insert("read-only".into(), vec![template.into()]);
        let spec = launch(&profile, "test", &agent("read-only")).unwrap();
        assert_eq!(spec.argv, ["test", BRIEFING, BRIEFING]);
        assert_eq!(spec.env["BRIEFING"], BRIEFING);
        assert_eq!(spec.files["/work/proj-abc/copy"], BRIEFING);
    }
}

#[test]
fn malformed_templates_in_each_renderable_field_are_config_invalid() {
    for field in [
        "command",
        "args",
        "args_json",
        "env",
        "path",
        "content",
        "builtin_tools",
    ] {
        let mut profile: HarnessProfile = toml::from_str("command = 'test'").unwrap();
        let bad = "{{ broken".to_string();
        match field {
            "command" => profile.command = bad,
            "args" => profile.args.push(bad),
            "args_json" => profile.args_json = Some(bad),
            "env" => {
                profile.env.insert("BAD".into(), bad);
            }
            "path" => {
                profile.files.insert(bad, "content".into());
            }
            "content" => {
                profile.files.insert("file".into(), bad);
            }
            "builtin_tools" => {
                profile.builtin_tools.insert("read-only".into(), vec![bad]);
            }
            _ => unreachable!(),
        }
        assert_eq!(
            launch(&profile, "test", &agent("read-only"))
                .unwrap_err()
                .code(),
            Code::ConfigInvalid,
            "{field}"
        );
    }
}

#[test]
fn agent_check_warns_for_legacy_placeholders_in_every_profile_field() {
    let dir = tempfile::tempdir().unwrap();
    let config = ConfigDir::new(dir.path());
    for field in [
        "command",
        "args",
        "args_json",
        "env",
        "path",
        "content",
        "builtin_tools",
        "model_args",
    ] {
        let mut profile: HarnessProfile = toml::from_str("command = 'test'").unwrap();
        match field {
            "command" => profile.command = "{issue}".into(),
            "args" => profile.args.push("{briefing}".into()),
            "args_json" => profile.args_json = Some(r#"["{issue}"]"#.into()),
            "model_args" => profile.model_args.push("{model}".into()),
            "env" => {
                profile.env.insert("OLD".into(), "{agent}".into());
            }
            "path" => {
                profile
                    .files
                    .insert("{session_dir}/file".into(), "content".into());
            }
            "content" => {
                profile.files.insert("file".into(), "{mcp_json}".into());
            }
            // No agent uses this level: checking must still inspect it.
            "builtin_tools" => {
                profile
                    .builtin_tools
                    .insert("none".into(), vec!["{trk_tools:mcp__trk_}".into()]);
            }
            _ => unreachable!(),
        }
        let mut user = seed_config();
        user.harness.clear();
        user.harness
            .insert("test".into(), toml::Value::try_from(profile).unwrap());
        let diagnostics = trk_agent::check::check_agents(&config, &user);
        assert!(diagnostics.iter().any(|d| d.level == Level::Warning && d.message.to_lowercase().contains("deprecat")), "{field}: {diagnostics:#?}");
    }
    let mut user = seed_config();
    user.harness.clear();
    user.harness.insert(
        "test".into(),
        toml::from_str::<toml::Value>(
            r#"
command = "test"
args = ['{{ briefing }}', '{"ordinary": "json"}', '{{ trk_env | tojson }}']
files = { "{{ session_dir }}/file" = '{{ {"key": issue} | tojson }}' }
"#,
        )
        .unwrap(),
    );
    let diagnostics = trk_agent::check::check_agents(&config, &user);
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.level == Level::Warning && d.message.to_lowercase().contains("deprecat")),
        "{diagnostics:#?}"
    );
}

#[test]
fn jinja_literals_comments_and_raw_blocks_are_not_legacy_tokens() {
    let mut profile: HarnessProfile = toml::from_str("command = 'test'").unwrap();
    profile.args = vec![
        r#"{{ {"a": {"b": 1}} | tojson }}|{{ "{issue}" }}|{{ "}}" }}|{issue}"#.into(),
        "{# {issue} #}{% raw %}{issue} {{ {% ' {% endraw %}|{issue}".into(),
    ];
    let spec = launch(&profile, "test", &agent("default")).unwrap();
    assert_eq!(spec.argv[1], r#"{"a":{"b":1}}|{issue}|}}|proj-abc"#);
    assert_eq!(spec.argv[2], "{issue} {{ {% ' |proj-abc");
}

#[test]
fn toml_filter_round_trips_quoted_keys_and_nested_values() {
    let mut profile: HarnessProfile = toml::from_str("command = 'test'").unwrap();
    profile.args = vec![
        r#"{{ {"a.b": {"quote": "a\"b\nc", "flags": [true, false], "number": 42}} | toml }}"#
            .into(),
    ];
    let spec = launch(&profile, "test", &agent("default")).unwrap();
    let value: toml::Value = toml::from_str(&format!("value = {}", spec.argv[1])).unwrap();
    assert_eq!(value["value"]["a.b"]["quote"].as_str(), Some("a\"b\nc"));
    assert_eq!(value["value"]["a.b"]["flags"][1].as_bool(), Some(false));
    assert_eq!(value["value"]["a.b"]["number"].as_integer(), Some(42));
}

#[test]
fn template_trailing_newlines_survive_in_launch_strings() {
    let mut profile: HarnessProfile = toml::from_str("command = 'test'").unwrap();
    profile.args = vec!["{{ issue }}\n".into()];
    profile.env.insert("TEXT".into(), "{{ issue }}\r\n".into());
    profile
        .files
        .insert("newlines".into(), "{{ issue }}\n\n".into());
    let spec = launch(&profile, "test", &agent("default")).unwrap();
    assert_eq!(spec.argv[1], "proj-abc\n");
    assert_eq!(spec.env["TEXT"], "proj-abc\r\n");
    assert_eq!(spec.files["/work/proj-abc/newlines"], "proj-abc\n\n");
}

#[test]
fn model_and_agent_arguments_share_single_pass_template_rendering() {
    let model = "literal {issue} {{ {%";
    let mut profile: HarnessProfile = toml::from_str("command = 'test'").unwrap();
    profile.model = Some(model.into());
    profile.model_args = vec!["--model".into(), "{{ model }}".into()];
    profile.args = vec!["base".into(), "{{ briefing }}".into()];
    profile.args_json = Some(r#"{{ ["first", model, ""] | tojson }}"#.into());
    profile
        .builtin_tools
        .insert("read-only".into(), vec!["{{ trk_tools|join(',') }}".into()]);
    let mut agent = agent("read-only");
    agent
        .harness
        .args
        .insert("test".into(), vec!["label={{ model }}".into()]);
    let spec = launch(&profile, "test", &agent).unwrap();
    assert_eq!(spec.model.as_deref(), Some(model));
    assert_eq!(
        spec.argv,
        [
            "test",
            "first",
            model,
            "",
            "base",
            "get_task,add_note",
            "--model",
            model,
            "label=literal {issue} {{ {%",
            BRIEFING
        ]
    );
}

#[test]
fn args_json_rejects_invalid_json_and_non_string_arguments() {
    for source in [
        "not json",
        r#"{"flag": true}"#,
        "[42]",
        "[null]",
        r#"["ok", []]"#,
    ] {
        let mut profile: HarnessProfile = toml::from_str("command = 'test'").unwrap();
        profile.args_json = Some(source.into());
        let error = launch(&profile, "test", &agent("default")).unwrap_err();
        assert_eq!(error.code(), Code::ConfigInvalid, "{source}");
        assert!(error.message().contains("args_json"), "{error}");
    }
}
