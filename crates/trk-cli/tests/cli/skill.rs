use super::*;

fn configure(e: &Env, path: &str, links: &[&str]) {
    let file = e.trk_dir().join("config.toml");
    let mut config: toml::Table = toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    config.insert(
        "skill".into(),
        toml::Value::try_from(json!({"path": path, "links": links})).unwrap(),
    );
    std::fs::write(file, toml::to_string(&config).unwrap()).unwrap();
    // Remove previous install artifacts so assertions exercise only this config.
    std::fs::remove_dir_all(e.repo().join(".agents/skills/trk")).unwrap();
    std::fs::write(e.repo().join(".git/info/exclude"), "").unwrap();
}

#[test]
fn custom_skill_path_and_extra_links_are_refreshed_and_excluded() {
    let e = Env::new();
    let repo = e.repo().canonicalize().unwrap();
    let path = "custom/skills/issue-tracker";
    let links = [".other/skills/trk", "nested/harness/skills/trk"];
    configure(&e, path, &links);
    assert_eq!(e.ok(&["whoami"])["skill"], "missing");
    let installed = e.ok(&["install"]);
    assert_eq!(installed["skill"]["path"], json!(repo.join(path)));
    let content = std::fs::read_to_string(repo.join(path).join("SKILL.md")).unwrap();
    assert!(content.starts_with("---\nname: trk\n"));
    for (i, link) in links.iter().enumerate() {
        assert!(repo.join(link).is_symlink());
        assert_eq!(
            std::fs::read_to_string(repo.join(link).join("SKILL.md")).unwrap(),
            content
        );
        assert_eq!(installed["skill"]["links"][i]["linked"], true);
        assert_eq!(
            installed["skill"]["links"][i]["path"],
            json!(repo.join(link))
        );
        let out = StdCommand::new("git")
            .current_dir(&repo)
            .args(["check-ignore", link])
            .output()
            .unwrap();
        assert!(out.status.success(), "{link} is not excluded");
    }
    let out = StdCommand::new("git")
        .current_dir(&repo)
        .args(["check-ignore", &format!("{path}/SKILL.md")])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!repo.join(".agents/skills/trk").exists());
    let exclude = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
    assert!(!exclude.contains(".agents"));
    assert!(!exclude.contains(".claude"));
    std::fs::write(repo.join(path).join("SKILL.md"), "outdated").unwrap();
    assert_eq!(e.ok(&["whoami"])["skill"], "outdated");
    e.ok(&["install"]);
    assert_eq!(e.ok(&["whoami"])["skill"], "current");
    assert_eq!(
        std::fs::read_to_string(repo.join(links[1]).join("SKILL.md")).unwrap(),
        content
    );
}

#[test]
fn configured_skill_with_no_links_ignores_claude_detection() {
    let e = Env::new();
    configure(&e, "custom/trk", &[]);
    std::fs::create_dir_all(e.repo().join(".claude")).unwrap();
    std::fs::create_dir_all(e.dir.path().join("home/.claude")).unwrap();
    let installed = e.ok(&["install"]);
    assert_eq!(installed["skill"]["links"], json!([]));
    assert!(e.repo().join("custom/trk/SKILL.md").is_file());
    assert!(!e.repo().join(".claude/skills/trk").exists());
    assert!(
        !std::fs::read_to_string(e.repo().join(".git/info/exclude"))
            .unwrap()
            .contains(".claude")
    );
}

#[test]
fn disabled_skill_installs_and_excludes_nothing() {
    let e = Env::new();
    configure(&e, "", &[".claude/skills/trk"]);
    let installed = e.ok(&["install"]);
    assert_eq!(installed["skill"]["state"], "disabled");
    assert_eq!(installed["skill"]["path"], Value::Null);
    assert_eq!(installed["skill"]["links"], json!([]));
    assert_eq!(e.ok(&["whoami"])["skill"], "disabled");
    assert!(!e.repo().join("SKILL.md").exists());
    assert!(!e.repo().join(".claude").exists());
    assert_eq!(
        std::fs::read_to_string(e.repo().join(".git/info/exclude")).unwrap(),
        ""
    );
}

#[test]
fn skill_warnings_follow_configured_paths_in_install_and_run() {
    for disabled in [false, true] {
        let e = Env::new();
        let repo = e.repo();
        configure(
            &e,
            if disabled { "" } else { "custom/trk" },
            &[".other/skills/trk"],
        );
        // Include a broken symlink: existence checks miss tracked links.
        std::fs::create_dir_all(repo.join(".other/skills")).unwrap();
        std::os::unix::fs::symlink(repo.join("custom/trk"), repo.join(".other/skills/trk"))
            .unwrap();
        for path in ["custom/trk/SKILL.md", ".agents/skills/trk/SKILL.md"] {
            std::fs::create_dir_all(repo.join(path).parent().unwrap()).unwrap();
            std::fs::write(repo.join(path), "tracked skill").unwrap();
        }
        for args in [
            vec!["add", "-f", "custom", ".other", ".agents"],
            vec![
                "-c",
                "user.email=t@e.st",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "tracked skills",
            ],
        ] {
            assert!(
                StdCommand::new("git")
                    .current_dir(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let installed = e.ok(&["install"]);
        std::fs::remove_dir_all(repo.join("custom/trk")).unwrap();
        let id = e.create(&["Check configured skill warnings"]);
        let run = e
            .cmd()
            .args(["--json", "run", &id, "--harness", "omp", "--print"])
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        for warnings in [
            installed["warnings"].to_string(),
            String::from_utf8(run.stderr).unwrap(),
        ] {
            assert_eq!(
                warnings.contains("custom/trk is tracked"),
                !disabled,
                "{warnings}"
            );
            assert_eq!(
                warnings.contains(".other/skills/trk is tracked"),
                !disabled,
                "{warnings}"
            );
            assert!(!warnings.contains(".agents/skills/trk"), "{warnings}");
        }
    }
}

#[test]
fn configured_skill_warnings_are_relative_to_the_nested_project() {
    let e = Env::new();
    let repo = e.repo();
    configure(&e, "custom/trk", &[".other/skills/trk"]);
    let project = repo.join("services/api");
    std::fs::create_dir_all(project.join("custom/trk")).unwrap();
    std::fs::create_dir_all(project.join(".other/skills")).unwrap();
    std::fs::rename(e.trk_dir(), project.join(".trk")).unwrap();
    std::fs::write(project.join("custom/trk/SKILL.md"), "tracked nested skill").unwrap();
    std::os::unix::fs::symlink("../../custom/trk", project.join(".other/skills/trk")).unwrap();
    for args in [
        vec!["add", "-f", "services/api/custom", "services/api/.other"],
        vec![
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "nested skill",
        ],
    ] {
        assert!(
            StdCommand::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let install = e
        .cmd()
        .current_dir(&project)
        .args(["--json", "install"])
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let installed: Value = serde_json::from_slice(&install.stdout).unwrap();
    let create = e
        .cmd()
        .current_dir(&project)
        .args(["--json", "create", "Nested skill warnings"])
        .output()
        .unwrap();
    assert!(
        create.status.success(),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );
    let created: Value = serde_json::from_slice(&create.stdout).unwrap();
    let id = created["data"]["id"].as_str().unwrap();
    let run = e
        .cmd()
        .current_dir(&project)
        .args(["--json", "run", id, "--harness", "omp", "--print"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    for warnings in [
        installed["data"]["warnings"].to_string(),
        String::from_utf8(run.stderr).unwrap(),
    ] {
        assert!(warnings.contains("custom/trk is tracked"), "{warnings}");
        assert!(
            warnings.contains(".other/skills/trk is tracked"),
            "{warnings}"
        );
    }
}
