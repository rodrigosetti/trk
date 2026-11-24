use super::*;

#[test]
fn install_preserves_legacy_files_and_issue_types_even_when_forced() {
    let e = Env::new();
    let legacy = [
        (
            "types/code-change.toml",
            "description = 'Legacy code change'\nagent = 'implementer'\nworktree = true\n",
        ),
        (
            "types/review.toml",
            "description = 'Legacy review'\nagent = 'reviewer'\nworktree = false\n",
        ),
        ("agents/reviewer.toml", "name = 'reviewer'\n"),
        (
            "prompts/reviewer.instructions.md.j2",
            "Legacy reviewer instructions",
        ),
    ];
    for (path, content) in legacy {
        std::fs::write(e.trk_dir().join(path), content).unwrap();
    }
    e.set_config(
        "default_type = \"change\"",
        "default_type = \"code-change\"",
    );
    let existing = e.create(&["Legacy issue"]);
    for (args, expected_default) in [
        (vec!["install"], "code-change"),
        (vec!["install", "--force"], "change"),
    ] {
        e.ok(&args);
        for (path, content) in legacy {
            assert_eq!(
                std::fs::read_to_string(e.trk_dir().join(path)).unwrap(),
                content
            );
        }
        assert_eq!(e.ok(&["whoami"])["default_type"], expected_default);
        assert_eq!(e.ok(&["show", &existing])["type"], "code-change");
        let id = e.create(&["Legacy default remains available"]);
        assert_eq!(e.ok(&["show", &id])["type"], expected_default);
    }
}

const DEFAULT_FILES: &[&str] = &[
    ".gitignore",
    "agents/implementer.toml",
    "agents/operator.toml",
    "agents/researcher.toml",
    "config.toml",
    "prompts/_landing.merge.md.j2",
    "prompts/_landing.pr.md.j2",
    "prompts/_rules.md.j2",
    "prompts/briefing.md.j2",
    "prompts/implementer.instructions.md.j2",
    "prompts/operator.instructions.md.j2",
    "prompts/researcher.instructions.md.j2",
    "types/bug.toml",
    "types/change.toml",
    "types/ops.toml",
    "types/research.toml",
];

fn sorted_paths(value: &Value) -> Vec<&str> {
    let mut paths: Vec<_> = value
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    paths.sort_unstable();
    paths
}

#[test]
fn install_presets_share_files_and_route_types_with_distinct_landings() {
    for (preset, landing) in [("local", "merge"), ("github", "pr")] {
        let e = Env::new();
        std::fs::remove_dir_all(e.trk_dir()).unwrap();
        let result = e.ok(&["install", "--preset", preset, "--prefix", "orch"]);
        assert_eq!(sorted_paths(&result["written"]), DEFAULT_FILES);
        assert_eq!(result["kept"], json!([]));
        assert_eq!(result["differ"], json!([]));
        assert_eq!(result["db_created"], true);
        assert!(!e.trk_dir().join("landing.txt").exists());
        assert!(!e.trk_dir().join("presets").exists());
        let config: toml::Value =
            toml::from_str(&std::fs::read_to_string(e.trk_dir().join("config.toml")).unwrap())
                .unwrap();
        assert_eq!(config["run"]["landing"].as_str(), Some(landing));
        assert_eq!(e.ok(&["whoami"])["default_type"], "change");
        for (kind, agent, worktree) in [
            ("change", "implementer", true),
            ("bug", "implementer", true),
            ("research", "researcher", false),
            ("ops", "operator", false),
        ] {
            let route: toml::Value = toml::from_str(
                &std::fs::read_to_string(e.trk_dir().join(format!("types/{kind}.toml"))).unwrap(),
            )
            .unwrap();
            assert_eq!(route["agent"].as_str(), Some(agent));
            assert_eq!(route["worktree"].as_bool(), Some(worktree));
            let id = e.create(&["--type", kind, kind]);
            assert_eq!(e.ok(&["show", &id])["type"], kind);
            if kind == "bug" {
                let preview = e.ok(&["agent", "preview", agent, "--issue", &id]);
                let guidance = preview["guidance"].as_str().unwrap().to_lowercase();
                for expected in ["reproduc", "root cause", "regression test"] {
                    assert!(guidance.contains(expected), "{guidance}");
                }
            }
            if kind == "ops" {
                assert!(route.get("guidance").is_none());
                let spec = e.ok(&["run", &id, "--harness", "omp", "--dry-run"]);
                assert_eq!(spec["agent"], "operator");
                assert_eq!(spec["routing"]["worktree"], false);
                assert!(spec["worktree"].is_null());
                assert_eq!(
                    Path::new(spec["cwd"].as_str().unwrap()),
                    e.repo().canonicalize().unwrap()
                );
                assert!(!e.trk_dir().join("worktrees").exists());
            }
        }
        e.ok(&["agent", "check"]);
        let id = e.create(&["Default type"]);
        assert_eq!(e.ok(&["show", &id])["type"], "change");
        let again = e.ok(&["install", "--preset", preset]);
        assert_eq!(again["written"], json!([]));
        assert_eq!(sorted_paths(&again["kept"]), DEFAULT_FILES);
        assert_eq!(again["differ"], json!([]));
    }
}

#[test]
fn install_custom_source_is_direct_recursive_binary_and_has_no_builtin_fallback() {
    let e = Env::new();
    let config = std::fs::read_to_string(e.trk_dir().join("config.toml"))
        .unwrap()
        .replace("email = \"alice@example.com\"", "{{EMAIL_LINE}}")
        .replace("prefix = \"orch\"", "prefix = \"{{PREFIX}}\"")
        .replace("landing = \"merge\"", "landing = {{LANDING}}")
        .replace("base = \"main\"", "base = {{BASE}}");
    assert!(config.contains("{{EMAIL_LINE}}"));
    assert!(config.contains("{{PREFIX}}"));
    assert!(config.contains("{{BASE}}"));
    let source = e.dir.path().join("custom-seed");
    std::fs::create_dir_all(source.join("assets/nested")).unwrap();
    std::fs::create_dir_all(source.join("prompts")).unwrap();
    std::fs::create_dir_all(source.join("presets/local")).unwrap();
    std::fs::write(source.join(".gitignore"), "*\n").unwrap();
    std::fs::write(source.join("config.toml"), config).unwrap();
    let binary = [0, 255, 128, 10];
    std::fs::write(source.join("assets/nested/logo.bin"), binary).unwrap();
    let prompt = "{{EMAIL_LINE}} {{PREFIX}} {{BASE}} {{LANDING}}\n";
    std::fs::write(source.join("prompts/custom.md.j2"), prompt).unwrap();
    std::fs::write(source.join("presets/local/ordinary.txt"), "not an overlay").unwrap();
    std::fs::remove_dir_all(e.trk_dir()).unwrap();
    let source_arg = source.to_str().unwrap();
    let expected = [
        ".gitignore",
        "assets/nested/logo.bin",
        "config.toml",
        "presets/local/ordinary.txt",
        "prompts/custom.md.j2",
    ];

    let installed = e.ok(&["install", "--from", source_arg, "--prefix", "custom"]);
    assert_eq!(sorted_paths(&installed["written"]), expected);
    assert_eq!(installed["kept"], json!([]));
    assert_eq!(installed["differ"], json!([]));
    assert!(
        !installed["warnings"]
            .to_string()
            .contains("skip verification")
    );
    assert!(!installed["warnings"].to_string().contains("gh auth login"));
    assert_eq!(
        std::fs::read(e.trk_dir().join("assets/nested/logo.bin")).unwrap(),
        binary
    );
    assert_eq!(
        std::fs::read_to_string(e.trk_dir().join("prompts/custom.md.j2")).unwrap(),
        prompt,
        "only config.toml substitutes install markers"
    );
    assert_eq!(
        std::fs::read_to_string(e.trk_dir().join("presets/local/ordinary.txt")).unwrap(),
        "not an overlay"
    );
    for path in DEFAULT_FILES.iter().filter(|path| !expected.contains(path)) {
        assert!(
            !e.trk_dir().join(path).exists(),
            "{path}: unexpected builtin fallback"
        );
    }
    let installed_config = std::fs::read_to_string(e.trk_dir().join("config.toml")).unwrap();
    for marker in ["{{EMAIL_LINE}}", "{{PREFIX}}", "{{BASE}}", "{{LANDING}}"] {
        assert!(!installed_config.contains(marker), "{marker}");
    }
    let parsed: toml::Table = toml::from_str(&installed_config).unwrap();
    assert_eq!(parsed["user"]["email"].as_str(), Some("alice@example.com"));
    assert_eq!(parsed["issues"]["prefix"].as_str(), Some("custom"));
    assert_eq!(parsed["run"]["base"].as_str(), Some("main"));

    // Project identity is not config drift; custom harness changes are.
    e.set_config("prefix = \"custom\"", "prefix = \"local\"");
    let unchanged = e.ok(&["install", "--from", source_arg]);
    assert_eq!(unchanged["written"], json!([]));
    assert_eq!(sorted_paths(&unchanged["kept"]), expected);
    assert_eq!(unchanged["differ"], json!([]));
    let mut updated: toml::Table = toml::from_str(
        &std::fs::read_to_string(source.join("config.toml"))
            .unwrap()
            .replace("{{EMAIL_LINE}}", "email = \"alice@example.com\"")
            .replace("{{PREFIX}}", "custom")
            .replace("{{LANDING}}", "\"merge\"")
            .replace("{{BASE}}", "\"main\""),
    )
    .unwrap();
    updated["harness"]["omp"]["command"] = toml::Value::String("custom-omp".into());
    let updated_config = toml::to_string(&updated).unwrap();
    std::fs::write(source.join("config.toml"), &updated_config).unwrap();
    std::fs::write(source.join("assets/nested/logo.bin"), [255, 0, 1]).unwrap();
    std::fs::write(source.join("prompts/custom.md.j2"), "updated prompt").unwrap();
    let kept = e.ok(&["install", "--from", source_arg]);
    assert_eq!(kept["written"], json!([]));
    assert_eq!(sorted_paths(&kept["kept"]), expected);
    assert_eq!(
        sorted_paths(&kept["differ"]),
        [
            "assets/nested/logo.bin",
            "config.toml",
            "prompts/custom.md.j2"
        ]
    );
    assert_eq!(
        std::fs::read(e.trk_dir().join("assets/nested/logo.bin")).unwrap(),
        binary
    );
    assert_eq!(
        std::fs::read_to_string(e.trk_dir().join("prompts/custom.md.j2")).unwrap(),
        prompt
    );
    assert_eq!(
        std::fs::read_to_string(e.trk_dir().join("config.toml")).unwrap(),
        installed_config.replace("prefix = \"custom\"", "prefix = \"local\"")
    );
    std::fs::write(e.trk_dir().join("local-only.txt"), "keep me").unwrap();
    let forced = e.ok(&["install", "--from", source_arg, "--force"]);
    assert_eq!(sorted_paths(&forced["written"]), expected);
    assert_eq!(forced["kept"], json!([]));
    assert_eq!(forced["differ"], json!([]));
    for path in expected {
        assert_eq!(
            std::fs::read(e.trk_dir().join(path)).unwrap(),
            std::fs::read(source.join(path)).unwrap(),
            "{path}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(e.trk_dir().join("local-only.txt")).unwrap(),
        "keep me",
        "--force overwrites selected files without pruning other content"
    );
}

#[test]
fn install_lists_presets_outside_a_project_without_writing() {
    let e = Env {
        dir: tempfile::tempdir().unwrap(),
    };
    std::fs::create_dir(e.repo()).unwrap();
    assert_eq!(
        e.ok(&["install", "--list-presets"]),
        json!({"presets": ["github", "local"]})
    );
    assert_eq!(std::fs::read_dir(e.repo()).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(e.dir.path()).unwrap().count(), 1);
}

#[test]
fn install_rejects_unknown_sources_presets_and_conflicting_options() {
    let e = Env::new();
    std::fs::remove_dir_all(e.trk_dir()).unwrap();
    let missing = e.dir.path().join("missing-seed");
    let file = e.dir.path().join("not-a-directory");
    std::fs::write(&file, "not a seed directory").unwrap();
    let empty = e.dir.path().join("empty-seed");
    std::fs::create_dir(&empty).unwrap();
    for args in [
        vec!["install", "--preset", "unknown"],
        vec!["install", "--preset", "default"],
        vec!["install", "--preset", "minimal"],
        vec!["install", "--from", missing.to_str().unwrap()],
        vec!["install", "--from", file.to_str().unwrap()],
    ] {
        e.err(&args, 2, "bad_args");
        assert!(!e.trk_dir().exists(), "{args:?} wrote an installation");
    }
    // Argument parsing errors use clap's stderr, not the runtime JSON envelope.
    for args in [
        vec![
            "install",
            "--from",
            empty.to_str().unwrap(),
            "--preset",
            "local",
        ],
        vec!["install", "--list-presets", "--force"],
        vec!["install", "--list-presets", "--prefix", "orch"],
        vec!["install", "--list-presets", "--preset", "github"],
        vec![
            "install",
            "--list-presets",
            "--from",
            empty.to_str().unwrap(),
        ],
        vec!["install", "--minimal", "--preset", "local"],
        vec!["install", "--minimal", "--from", empty.to_str().unwrap()],
        vec!["install", "--minimal", "--list-presets"],
        vec!["install", "--preset"],
        vec!["install", "--from"],
    ] {
        let output = e.cmd().arg("--json").args(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("error:"),
            "{args:?}: {output:?}"
        );
        assert!(!e.trk_dir().exists(), "{args:?} wrote an installation");
    }
}

#[test]
fn install_custom_source_rejects_symlink_files_and_directories() {
    let e = Env::new();
    let source = e.dir.path().join("custom-seed");
    std::fs::create_dir(&source).unwrap();
    std::fs::copy(e.trk_dir().join("config.toml"), source.join("config.toml")).unwrap();
    let outside = e.dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("private.txt"), "not seed content").unwrap();
    std::fs::remove_dir_all(e.trk_dir()).unwrap();

    let link = source.join("linked");
    for target in [outside.join("private.txt"), outside.clone()] {
        std::os::unix::fs::symlink(target, &link).unwrap();
        e.err(
            &["install", "--from", source.to_str().unwrap()],
            2,
            "bad_args",
        );
        assert!(
            !e.trk_dir().exists(),
            "invalid source wrote an installation"
        );
        std::fs::remove_file(&link).unwrap();
    }
}
