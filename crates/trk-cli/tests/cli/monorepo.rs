use super::*;

fn git(repo: &Path, args: &[&str]) -> String {
    let out = StdCommand::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn success(mut command: Command, cwd: &Path, args: &[&str]) -> Value {
    let out = command
        .current_dir(cwd)
        .arg("--json")
        .args(args)
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(out.status.success(), "{args:?}: {value:#}");
    value["data"].clone()
}

fn project_workflow(relative: &str, external: bool) {
    let e = Env::new();
    let repo = e.repo().canonicalize().unwrap();
    let project = repo.join(relative);
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/example"), "tracked project\n").unwrap();
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "project sources",
        ],
    );
    // Explicit installation allows a project below an existing root project.
    success(e.cmd(), &project, &["--dir", ".trk", "install"]);
    let trk = project.join(".trk");
    let config = trk.join("config.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text = text.replace("links = []", "links = [\".claude/skills/trk\"]");
    if external {
        let outside = e.dir.path().canonicalize().unwrap().join("external-worktrees");
        text = text.replace(
            "worktrees = \".trk/worktrees\"",
            &format!("worktrees = {:?}", outside.to_str().unwrap()),
        );
    }
    text.push_str(
        "\n[harness.cwd-proof]\ncommand = 'sh'\n\
         args = ['-c', 'pwd > \"{{session_dir}}/observed-cwd\"']\n\
         files = { '.omp/mcp.json' = '{{ mcp_servers | tojson }}' }\n",
    );
    std::fs::write(config, text).unwrap();
    success(e.cmd(), &project, &["--dir", ".trk", "install"]);
    for path in [".agents/skills/trk/SKILL.md", ".claude/skills/trk"] {
        git(&project, &["check-ignore", path]);
    }
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");

    let issue = success(e.cmd(), &project, &["create", "Exercise project worktree"]);
    let id = issue["id"].as_str().unwrap();
    let preview = success(
        e.cmd(),
        &project,
        &["run", id, "--harness", "cwd-proof", "--dry-run"],
    );
    let worktree = PathBuf::from(preview["worktree"].as_str().unwrap());
    let cwd = if relative.is_empty() {
        worktree.clone()
    } else {
        worktree.join(relative)
    };
    assert_eq!(preview["cwd"], json!(cwd));
    let launched = success(
        e.cmd(),
        &project,
        &["run", id, "--harness", "cwd-proof", "--fg"],
    );
    assert_eq!(launched["exit_code"], 0);
    assert!(worktree.join(".git").is_file());
    assert!(cwd.join("src/example").is_file());
    assert_eq!(git(&worktree, &["branch", "--show-current"]), id);

    // No TRK_DIR: discovery must map the session's path to the primary project,
    // including when a root project exists and when worktrees live outside it.
    let shown = success(e.cmd(), &cwd, &["show", id]);
    assert_eq!(shown["state"], "open");
    assert!(shown["claim"].is_null());
    let actor = launched["actor"].as_str().unwrap();
    let observed = trk
        .join("sessions")
        .join(actor.rsplit(':').next().unwrap())
        .join("observed-cwd");
    assert_eq!(
        std::fs::read_to_string(observed).unwrap().trim(),
        cwd.to_str().unwrap()
    );
    assert_eq!(git(&worktree, &["status", "--porcelain"]), "");

    // A manual claim from below the session root must still record that root.
    let manual = success(e.cmd(), &cwd, &["create", "Manual claim"]);
    let manual_id = manual["id"].as_str().unwrap();
    let claim = success(e.cmd(), &cwd, &["claim", manual_id]);
    assert_eq!(claim["claim"]["worktree"], json!(worktree));
    success(e.cmd(), &cwd, &["unclaim", manual_id]);

    let check = format!(
        "test \"$TRK_REPO\" = '{}' && test \"$TRK_WORKTREE\" = '{}' && \
         test \"$(pwd)\" = \"$TRK_WORKTREE\" && test \"$TRK_BRANCH\" = '{}' && \
         test -f '{}/src/example'",
        repo.display(),
        worktree.display(),
        id,
        if relative.is_empty() { "." } else { relative },
    );
    let claim = success(e.cmd(), &cwd, &["--as", actor, "claim", id]);
    assert_eq!(claim["claim"]["worktree"], json!(worktree));
    success(
        e.cmd(),
        &cwd,
        &[
            "--as",
            actor,
            "wait",
            id,
            "--reason",
            "verify layout",
            "--check",
            &check,
        ],
    );
    let checked = success(e.cmd(), &project, &["check", id]);
    assert_eq!(checked["results"][0]["result"], "passed", "{checked:#}");
    assert_eq!(success(e.cmd(), &project, &["show", id])["state"], "closed");

    let protected = success(e.cmd(), &cwd, &["gc", id]);
    assert_eq!(protected["issues"][0]["worktree"]["status"], "skipped");
    assert!(worktree.exists());
    let collected = success(e.cmd(), &project, &["gc", id]);
    assert_eq!(
        collected["issues"][0]["worktree"]["status"], "removed",
        "{collected:#}"
    );
    assert_eq!(
        collected["issues"][0]["branch"]["status"], "removed",
        "{collected:#}"
    );
    assert!(!worktree.exists());
}

#[test]
fn subdirectory_projects_run_check_and_gc() {
    for external in [false, true] {
        project_workflow("services/api", external);
    }
}

#[test]
fn repository_root_projects_keep_run_check_and_gc_behavior() {
    for external in [false, true] {
        project_workflow("", external);
    }
}
