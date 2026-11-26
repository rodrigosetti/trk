use super::*;

#[test]
fn commands_require_an_installed_project() {
    let dir = tempfile::tempdir().unwrap();
    let bare = dir.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    let trk = |args: &[&str]| {
        let out = Command::cargo_bin("trk")
            .unwrap()
            .current_dir(&bare)
            .env("HOME", dir.path().join("home"))
            .env("TRK_USER_DIR", dir.path().join("user-config"))
            .env("TRK_ACTOR", "human:alice@example.com")
            .env_remove("TRK_DIR")
            .env_remove("TRK_FORMAT")
            .arg("--json")
            .args(args)
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        (out.status.code().unwrap(), v)
    };
    for args in [
        &["list"][..],
        &["create", "x"],
        &["whoami"],
        &["agent", "list"],
        &["check"],
    ] {
        let (code, v) = trk(args);
        assert_eq!(code, 3, "{args:?}: {v:#}");
        assert_eq!(v["error"]["code"], "not_installed", "{args:?}");
        assert!(
            v["error"]["hint"].as_str().unwrap().contains("trk install"),
            "{v:#}"
        );
    }
    // Nothing was created by the refused commands.
    assert!(!bare.join(".trk").exists());

    // Outside git, install uses the working directory; the prefix comes
    // from its name.
    let (code, v) = trk(&["install"]);
    assert_eq!(code, 0, "{v:#}");
    assert_eq!(v["data"]["prefix"], "bare");
    assert_eq!(v["data"]["db_created"], true);
    assert!(bare.join(".trk/trk.db").is_file());
    let (_, v) = trk(&["create", "First"]);
    assert!(v["data"]["id"].as_str().unwrap().starts_with("bare-"));
    // The removed commands and scopes are gone.
    for args in [
        &["init"][..],
        &["project", "list"],
        &["repo", "list"],
        &["skill", "install"],
        &["list", "--all-projects"],
        &["--project", "x", "list"],
    ] {
        let out = Command::cargo_bin("trk")
            .unwrap()
            .current_dir(&bare)
            .env("TRK_USER_DIR", dir.path().join("user-config"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
    let (code, v) = trk(&["list", "project=bare"]);
    assert_eq!((code, v["error"]["code"].as_str()), (2, Some("bad_query")));
}

#[test]
fn install_writes_the_project_directory_and_the_skill() {
    let e = Env::named();
    let repo = e.repo();
    let dir = e.trk_dir();
    // Everything is in .trk/, written explicitly, and ignored by git.
    for f in [
        ".gitignore",
        "trk.db",
        "config.toml",
        "types/change.toml",
        "agents/implementer.toml",
        "prompts/briefing.md.j2",
    ] {
        assert!(dir.join(f).is_file(), "{f}");
    }
    let config = std::fs::read_to_string(dir.join("config.toml")).unwrap();
    for setting in [
        "prefix = \"orch\"",
        "email = \"alice@example.com\"",
        "agent_lease = \"30m\"",
        "interval = \"5m\"",
        "timeout  = \"10m\"",
        "worktrees = \".trk/worktrees\"",
    ] {
        assert!(config.contains(setting), "config.toml lacks {setting}");
    }
    // The skill comes from the binary and is kept out of git.
    let skill = repo.join(".agents/skills/trk");
    let installed = std::fs::read_to_string(skill.join("SKILL.md")).unwrap();
    assert!(installed.starts_with("---\nname: trk\n"), "{installed}");
    assert!(skill.join("references/reference.md").is_file());
    assert!(!repo.join(".claude").exists(), "no Claude Code here");
    let status = StdCommand::new("git")
        .current_dir(&repo)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&status.stdout), "");

    // Again: config is kept, edits are reported, the skill is refreshed.
    let id = e.create(&["Survives a second install"]);
    std::fs::write(skill.join("SKILL.md"), "stale").unwrap();
    std::fs::write(dir.join("prompts/briefing.md.j2"), "mine").unwrap();
    assert_eq!(e.ok(&["whoami"])["skill"], "outdated");
    let v = e.ok(&["install"]);
    assert_eq!(v["db_created"], false);
    assert_eq!(v["written"], json!([]));
    assert_eq!(v["differ"], json!(["prompts/briefing.md.j2"]));
    assert_eq!(v["skill"]["state"], "current");
    assert_eq!(e.ok(&["show", &id])["id"], json!(id));
    assert_eq!(
        std::fs::read_to_string(dir.join("prompts/briefing.md.j2")).unwrap(),
        "mine"
    );
    // A prefix is only taken when the config is written.
    e.err(&["install", "--prefix", "other"], 2, "bad_args");
    e.err(
        &["install", "--prefix", "Bad-1", "--force"],
        4,
        "config_invalid",
    );
    let v = e.ok(&["install", "--force", "--prefix", "other"]);
    assert!(
        v["written"]
            .as_array()
            .unwrap()
            .contains(&json!("prompts/briefing.md.j2"))
    );
    assert!(e.create(&["New prefix"]).starts_with("other-"));
    assert_eq!(e.ok(&["show", &id])["id"], json!(id), "old ids remain");

    // Harness installation does not change explicitly configured skill links.
    std::fs::create_dir_all(e.dir.path().join("home/.claude")).unwrap();
    let v = e.ok(&["install"]);
    assert_eq!(v["skill"]["links"], json!([]));
    assert!(!repo.join(".claude/skills/trk").exists());
}

#[test]
fn install_reports_outdated_codex_profile_without_overwriting_config() {
    let e = Env::named();
    let path = e.trk_dir().join("config.toml");
    let seed = std::fs::read_to_string(&path).unwrap();
    // Project identity and formatting alone are not profile drift.
    let customized = format!(
        "# project settings\n{}",
        seed.replace("prefix = \"orch\"", "prefix = \"custom\"")
    );
    std::fs::write(&path, &customized).unwrap();
    assert_eq!(e.ok(&["install"])["differ"], json!([]));
    let mut config: toml::Table = toml::from_str(&customized).unwrap();
    config["harness"]["codex"]["command"] = toml::Value::String("custom-codex".into());
    let outdated = toml::to_string(&config).unwrap();
    std::fs::write(&path, &outdated).unwrap();
    let result = e.ok(&["install"]);
    assert_eq!(result["differ"], json!(["config.toml"]));
    assert!(
        result["kept"]
            .as_array()
            .unwrap()
            .contains(&json!("config.toml"))
    );
    assert!(
        !result["written"]
            .as_array()
            .unwrap()
            .contains(&json!("config.toml"))
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), outdated);
}

#[test]
fn the_project_is_found_from_subdirectories_worktrees_and_trk_dir() {
    let e = Env::new();
    let id = e.create(&["Visible everywhere"]);
    let sub = e.repo().join("a/b");
    std::fs::create_dir_all(&sub).unwrap();
    // A worktree outside the project does not contain the ignored .trk/.
    let outside = e.dir.path().join("outside");
    assert!(
        StdCommand::new("git")
            .current_dir(e.repo())
            .args(["worktree", "add", "-q", "-b", "side"])
            .arg(&outside)
            .status()
            .unwrap()
            .success()
    );
    assert!(!outside.join(".trk").exists());
    for cwd in [&sub, &outside] {
        let out = e
            .cmd()
            .current_dir(cwd)
            .args(["--json", "list"])
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["data"][0]["id"], json!(id), "{}: {v:#}", cwd.display());
    }
    // Installing from a worktree or a subdirectory targets the same project.
    let out = e
        .cmd()
        .current_dir(&outside)
        .args(["--json", "install"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["db_created"], false, "{v:#}");
    assert!(!outside.join(".trk").exists());
    // TRK_DIR reaches it from an unrelated directory.
    let out = e
        .cmd()
        .current_dir(e.dir.path())
        .env("TRK_DIR", e.trk_dir())
        .args(["--json", "show", &id])
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = e
        .cmd()
        .current_dir(e.dir.path())
        .args(["--json", "show", &id])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
}
