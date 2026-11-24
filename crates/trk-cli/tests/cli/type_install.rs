use super::*;

#[test]
fn install_reports_old_research_and_review_defaults_without_overwriting() {
    let e = Env::new();
    let old_types = [
        (
            "types/research.toml",
            "name = \"research\"\ndescription = \"research\"\nagent = \"researcher\"\n",
        ),
        (
            "types/review.toml",
            "name = \"review\"\ndescription = \"review\"\nagent = \"reviewer\"\n",
        ),
    ];
    let code_change = e.trk_dir().join("types/code-change.toml");
    let original_code_change = std::fs::read_to_string(&code_change).unwrap();
    for (relative, old) in old_types {
        let path = e.trk_dir().join(relative);
        let installed = std::fs::read_to_string(&path).unwrap();
        assert!(installed.contains("worktree = false"), "{relative}");
        std::fs::write(path, old).unwrap();
    }

    let installed = e.ok(&["install"]);
    assert_eq!(
        installed["differ"],
        json!(["types/research.toml", "types/review.toml"])
    );
    for (relative, old) in old_types {
        assert!(
            installed["kept"]
                .as_array()
                .unwrap()
                .contains(&json!(relative))
        );
        assert!(
            !installed["written"]
                .as_array()
                .unwrap()
                .contains(&json!(relative))
        );
        assert_eq!(
            std::fs::read_to_string(e.trk_dir().join(relative)).unwrap(),
            old
        );
    }
    assert_eq!(
        std::fs::read_to_string(code_change).unwrap(),
        original_code_change
    );
}

const DEFAULT_FILES: &[&str] = &[
    ".gitignore",
    "agents/implementer.toml",
    "agents/researcher.toml",
    "agents/reviewer.toml",
    "config.toml",
    "prompts/_rules.md.j2",
    "prompts/briefing.md.j2",
    "prompts/implementer.instructions.md.j2",
    "prompts/researcher.instructions.md.j2",
    "prompts/reviewer.instructions.md.j2",
    "types/code-change.toml",
    "types/research.toml",
    "types/review.toml",
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
fn install_explicit_default_preset_preserves_the_original_seed_set() {
    let e = Env::new();
    let original: Vec<_> = DEFAULT_FILES
        .iter()
        .map(|path| std::fs::read(e.trk_dir().join(path)).unwrap())
        .collect();
    std::fs::remove_dir_all(e.trk_dir()).unwrap();

    let result = e.ok(&["install", "--preset", "default", "--prefix", "orch"]);
    assert_eq!(sorted_paths(&result["written"]), DEFAULT_FILES);
    assert_eq!(result["kept"], json!([]));
    assert_eq!(result["differ"], json!([]));
    assert_eq!(result["db_created"], true);
    for (path, content) in DEFAULT_FILES.iter().zip(original) {
        assert_eq!(
            std::fs::read(e.trk_dir().join(path)).unwrap(),
            content,
            "{path}"
        );
    }
}

#[test]
fn install_minimal_preset_has_only_implementer_and_code_change() {
    let e = Env::new();
    std::fs::remove_dir_all(e.trk_dir()).unwrap();

    let result = e.ok(&["install", "--preset", "minimal", "--prefix", "orch"]);
    let expected = [
        ".gitignore",
        "agents/implementer.toml",
        "config.toml",
        "prompts/_rules.md.j2",
        "prompts/briefing.md.j2",
        "prompts/implementer.instructions.md.j2",
        "types/code-change.toml",
    ];
    assert_eq!(sorted_paths(&result["written"]), expected);
    assert_eq!(result["kept"], json!([]));
    assert_eq!(result["differ"], json!([]));
    for path in expected {
        assert!(e.trk_dir().join(path).is_file(), "{path}");
    }
    for path in DEFAULT_FILES.iter().filter(|path| !expected.contains(path)) {
        assert!(!e.trk_dir().join(path).exists(), "{path}");
    }
    let id = e.create(&["Minimal install supports its default issue type"]);
    assert_eq!(e.ok(&["show", &id])["type"], "code-change");
    let again = e.ok(&["install", "--preset", "minimal"]);
    assert_eq!(again["written"], json!([]));
    assert_eq!(sorted_paths(&again["kept"]), expected);
    assert_eq!(again["differ"], json!([]));
}

#[test]
fn install_custom_source_is_direct_recursive_binary_and_has_no_builtin_fallback() {
    let e = Env::new();
    let config = std::fs::read_to_string(e.trk_dir().join("config.toml"))
        .unwrap()
        .replace("email = \"alice@example.com\"", "{{EMAIL_LINE}}")
        .replace("prefix = \"orch\"", "prefix = \"{{PREFIX}}\"")
        .replace("base = \"main\"", "base = {{BASE}}");
    assert!(config.contains("{{EMAIL_LINE}}"));
    assert!(config.contains("{{PREFIX}}"));
    assert!(config.contains("{{BASE}}"));
    let source = e.dir.path().join("custom-seed");
    std::fs::create_dir_all(source.join("assets/nested")).unwrap();
    std::fs::create_dir_all(source.join("prompts")).unwrap();
    std::fs::create_dir_all(source.join("presets/default")).unwrap();
    std::fs::write(source.join(".gitignore"), "*\n").unwrap();
    std::fs::write(source.join("config.toml"), config).unwrap();
    let binary = [0, 255, 128, 10];
    std::fs::write(source.join("assets/nested/logo.bin"), binary).unwrap();
    let prompt = "{{EMAIL_LINE}} {{PREFIX}} {{BASE}}\n";
    std::fs::write(source.join("prompts/custom.md.j2"), prompt).unwrap();
    std::fs::write(
        source.join("presets/default/ordinary.txt"),
        "not an overlay",
    )
    .unwrap();
    std::fs::remove_dir_all(e.trk_dir()).unwrap();
    let source_arg = source.to_str().unwrap();
    let expected = [
        ".gitignore",
        "assets/nested/logo.bin",
        "config.toml",
        "presets/default/ordinary.txt",
        "prompts/custom.md.j2",
    ];

    let installed = e.ok(&["install", "--from", source_arg, "--prefix", "custom"]);
    assert_eq!(sorted_paths(&installed["written"]), expected);
    assert_eq!(installed["kept"], json!([]));
    assert_eq!(installed["differ"], json!([]));
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
        std::fs::read_to_string(e.trk_dir().join("presets/default/ordinary.txt")).unwrap(),
        "not an overlay"
    );
    for path in DEFAULT_FILES.iter().filter(|path| !expected.contains(path)) {
        assert!(
            !e.trk_dir().join(path).exists(),
            "{path}: unexpected builtin fallback"
        );
    }
    let installed_config = std::fs::read_to_string(e.trk_dir().join("config.toml")).unwrap();
    for marker in ["{{EMAIL_LINE}}", "{{PREFIX}}", "{{BASE}}"] {
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
        json!({"presets": ["default", "minimal"]})
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
            "default",
        ],
        vec!["install", "--list-presets", "--force"],
        vec!["install", "--list-presets", "--prefix", "orch"],
        vec!["install", "--list-presets", "--preset", "minimal"],
        vec![
            "install",
            "--list-presets",
            "--from",
            empty.to_str().unwrap(),
        ],
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
