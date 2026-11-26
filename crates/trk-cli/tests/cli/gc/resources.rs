use super::*;

#[test]
fn gc_removes_merged_done_resources_and_records_the_calling_actor() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    fixture.git_at(
        &path,
        &[
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "completed work",
        ],
    );
    fixture.git_at(&fixture.e.repo(), &["merge", "--ff-only", &id]);
    let before = fixture.notes(&id).as_array().unwrap().len();

    let data = fixture.run(&["gc", &id]);
    assert_eq!(data["dry_run"], false);
    let issue = gc_issue(&data, &id);
    for resource in ["session", "worktree", "branch"] {
        gc_status(issue, resource, "removed");
    }
    assert_eq!(issue["session"]["items"][0]["terminal"], "term-target");
    assert_eq!(issue["session"]["items"][0]["space"], "target");
    assert_eq!(
        issue["worktree"]["items"][0]["path"],
        path.to_str().unwrap()
    );
    assert!(!path.exists());
    assert!(!fixture.branch_exists(&id));
    assert_eq!(fixture.closed_spaces(), ["target"]);
    let notes = fixture.notes(&id);
    let added = &notes.as_array().unwrap()[before..];
    assert!(!added.is_empty(), "{notes:#}");
    assert!(
        added
            .iter()
            .all(|note| note["actor"] == "human:alice@example.com")
    );
}

#[test]
fn gc_dirty_worktree_gates_the_branch_but_not_the_session() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    std::fs::write(path.join("untracked.txt"), "keep this work").unwrap();

    let data = fixture.run(&["gc", &id]);
    let issue = gc_issue(&data, &id);
    gc_status(issue, "session", "removed");
    gc_status(issue, "worktree", "skipped");
    gc_status(issue, "branch", "skipped");
    assert_eq!(
        std::fs::read_to_string(path.join("untracked.txt")).unwrap(),
        "keep this work"
    );
    assert!(fixture.branch_exists(&id));
    assert_eq!(fixture.closed_spaces(), ["target"]);
}

#[test]
fn gc_keeps_an_unmerged_branch_after_removing_its_worktree() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    fixture.git_at(
        &path,
        &[
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "unmerged work",
        ],
    );

    let data = fixture.run(&["gc", &id]);
    let issue = gc_issue(&data, &id);
    gc_status(issue, "worktree", "removed");
    gc_status(issue, "branch", "skipped");
    assert!(!path.exists());
    assert!(fixture.branch_exists(&id));
}

#[test]
fn gc_only_collects_abandoned_issues_when_requested() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("abandoned", "term-target");
    let notes = fixture.notes(&id);

    fixture.run(&["gc", &id]);
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
    assert!(fixture.closed_spaces().is_empty());
    assert_eq!(fixture.notes(&id), notes);

    let data = fixture.run(&["gc", &id, "--abandoned"]);
    let issue = gc_issue(&data, &id);
    for resource in ["session", "worktree", "branch"] {
        gc_status(issue, resource, "removed");
    }
    assert!(!path.exists());
    assert!(!fixture.branch_exists(&id));
}

#[test]
fn gc_never_collects_claimed_or_waiting_issues_even_with_explicit_ids() {
    let fixture = GcEnv::new();
    let claimed = fixture.e.create(&["Claimed issue"]);
    let claimed_path = fixture.worktree(&claimed);
    fixture.claim(&claimed, Some(&claimed_path), "term-target", &fixture.host);
    let waiting = fixture.e.create(&["Waiting issue"]);
    let waiting_path = fixture.worktree(&waiting);
    fixture.claim(&waiting, Some(&waiting_path), "term-history", &fixture.host);
    fixture
        .e
        .ok(&["wait", &waiting, "--reason", "human decision"]);
    let claimed_notes = fixture.notes(&claimed);
    let waiting_notes = fixture.notes(&waiting);

    fixture.run(&["gc", &claimed, &waiting, "--abandoned"]);
    for (id, path) in [(&claimed, &claimed_path), (&waiting, &waiting_path)] {
        assert!(path.exists());
        assert!(fixture.branch_exists(id));
    }
    assert!(fixture.closed_spaces().is_empty());
    assert_eq!(fixture.notes(&claimed), claimed_notes);
    assert_eq!(fixture.notes(&waiting), waiting_notes);
    assert_eq!(fixture.e.ok(&["show", &claimed])["state"], "claimed");
    assert_eq!(fixture.e.ok(&["show", &waiting])["state"], "waiting");
}

#[test]
fn gc_keeps_worktrees_outside_the_configured_directory() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Closed issue"]);
    let outside = fixture.e.dir.path().join("outside-worktree");
    let path = fixture.worktree_at(&id, &outside);
    fixture.claim(&id, Some(&path), "term-target", &fixture.host);
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
    let preview = fixture.run(&["gc", &id, "--dry-run"]);
    let issue = gc_issue(&preview, &id);
    gc_status(issue, "worktree", "skipped");
    gc_status(issue, "branch", "skipped");
    assert!(path.is_dir());
    assert!(fixture.branch_exists(&id));

    let data = fixture.run(&["gc", &id]);
    let issue = gc_issue(&data, &id);
    gc_status(issue, "worktree", "skipped");
    gc_status(issue, "branch", "skipped");
    gc_status(issue, "session", "removed");
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
    assert!(
        issue["worktree"]["reason"]
            .as_str()
            .unwrap()
            .contains("outside the configured worktrees directory")
    );

    // A new check process must still warn about genuinely protected resources.
    let output = fixture
        .cmd()
        .env("TRK_NOW", "2026-11-20T12:00:00Z")
        .arg("check")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains(&id), "{stdout}");
    assert!(stdout.contains("outside the configured"), "{stdout}");
}

#[test]
fn gc_does_not_touch_resources_recorded_on_a_foreign_host() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Closed issue"]);
    let path = fixture.worktree(&id);
    let foreign_host = format!("{}.not-this-host.invalid", fixture.host);
    fixture.claim(&id, Some(&path), "term-target", &foreign_host);
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
    let notes = fixture.notes(&id);

    let data = fixture.run(&["gc", &id]);
    let issue = gc_issue(&data, &id);
    for resource in ["session", "worktree", "branch"] {
        gc_status(issue, resource, "skipped");
    }
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
    assert!(fixture.closed_spaces().is_empty());
    assert_eq!(fixture.notes(&id), notes);
}

#[test]
fn gc_never_removes_its_current_worktree() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    let mut command = fixture.cmd();
    command.current_dir(&path);

    let data = gc_success(command, &["gc", &id]);
    let issue = gc_issue(&data, &id);
    gc_status(issue, "session", "removed");
    gc_status(issue, "worktree", "skipped");
    gc_status(issue, "branch", "skipped");
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
}

#[test]
fn gc_keeps_locked_worktrees_and_their_branches() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "missing-terminal");
    fixture.git_at(
        &fixture.e.repo(),
        &["worktree", "lock", path.to_str().unwrap()],
    );

    for args in [&["gc", &id, "--dry-run"][..], &["gc", &id][..]] {
        let data = fixture.run(args);
        let issue = gc_issue(&data, &id);
        gc_status(issue, "worktree", "skipped");
        gc_status(issue, "branch", "skipped");
        assert!(
            issue["worktree"]["reason"]
                .as_str()
                .unwrap()
                .contains("locked")
        );
        assert!(path.is_dir());
        assert!(fixture.branch_exists(&id));
    }
}

#[test]
fn gc_protects_the_callers_entire_space_not_just_its_terminal() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-current-target");
    let mut command = fixture.cmd();
    command.env("HC_TERMINAL_ID", "term-current");

    let data = gc_success(command, &["gc", &id]);
    let issue = gc_issue(&data, &id);
    gc_status(issue, "session", "skipped");
    gc_status(issue, "worktree", "removed");
    assert!(fixture.closed_spaces().is_empty());
    assert!(!path.exists());
}

#[test]
fn gc_protects_spaces_shared_with_another_claimed_or_waiting_issue() {
    for state in ["claimed", "waiting"] {
        let fixture = GcEnv::new();
        let (closed, path) = fixture.closed("done", "term-shared-closed");
        let active = fixture.e.create(&["Protected issue"]);
        fixture.claim(&active, None, "term-shared-active", &fixture.host);
        if state == "waiting" {
            fixture
                .e
                .ok(&["wait", &active, "--reason", "human decision"]);
        }

        let data = fixture.run(&["gc", &closed]);
        let issue = gc_issue(&data, &closed);
        gc_status(issue, "session", "skipped");
        gc_status(issue, "worktree", "removed");
        assert!(!path.exists());
        assert!(fixture.closed_spaces().is_empty());
        assert_eq!(fixture.e.ok(&["show", &active])["state"], state);
    }
}

#[test]
fn gc_second_run_is_absent_and_does_not_duplicate_notes() {
    let fixture = GcEnv::new();
    let (id, _) = fixture.closed("done", "term-target");
    fixture.run(&["gc", &id]);
    let notes = fixture.notes(&id);
    let closes = fixture.closed_spaces();

    let data = fixture.run(&["gc", &id]);
    let issue = gc_issue(&data, &id);
    for resource in ["session", "worktree", "branch"] {
        gc_status(issue, resource, "absent");
    }
    assert_eq!(fixture.notes(&id), notes);
    assert_eq!(fixture.closed_spaces(), closes);
}

#[test]
fn gc_older_than_filters_by_close_time() {
    let fixture = GcEnv::new();
    let (old, old_path) = fixture.closed("done", "term-target");
    let recent = fixture.e.create(&["Recently closed"]);
    let recent_path = fixture.worktree(&recent);
    fixture.claim(&recent, Some(&recent_path), "term-history", &fixture.host);
    fixture.close_at(&recent, "done", "2026-11-22T09:00:00Z");
    let notes = fixture.notes(&recent);
    let mut command = fixture.cmd();
    command.env("TRK_NOW", "2026-11-22T10:00:00Z");

    let data = gc_success(command, &["gc", "--older-than", "24h"]);
    gc_status(gc_issue(&data, &old), "worktree", "removed");
    assert!(!old_path.exists());
    assert!(!fixture.branch_exists(&old));
    assert!(recent_path.exists());
    assert!(fixture.branch_exists(&recent));
    assert_eq!(fixture.closed_spaces(), ["target"]);
    assert_eq!(fixture.notes(&recent), notes);
}

#[test]
fn gc_dry_run_reports_removals_without_changing_resources_or_notes() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    let notes = fixture.notes(&id);

    let data = fixture.run(&["gc", &id, "--dry-run"]);
    assert_eq!(data["dry_run"], true);
    let issue = gc_issue(&data, &id);
    for resource in ["session", "worktree", "branch"] {
        gc_status(issue, resource, "removed");
        assert!(
            issue[resource]["reason"]
                .as_str()
                .unwrap()
                .starts_with("would remove "),
            "{issue:#}"
        );
    }
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
    assert!(fixture.closed_spaces().is_empty());
    assert_eq!(fixture.notes(&id), notes);
}

#[test]
fn gc_collects_every_historical_local_claim_not_only_the_latest() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Closed issue"]);
    fixture.claim(&id, Some(&fixture.e.repo()), "term-history", &fixture.host);
    fixture.e.ok(&["unclaim", &id]);
    let first = fixture.worktree(&id);
    fixture.claim(&id, Some(&first), "term-history", &fixture.host);
    fixture.e.ok(&["unclaim", &id]);
    let second_branch = format!("{id}-second");
    let second = fixture.worktree_at(
        &second_branch,
        &fixture.e.trk_dir().join("worktrees").join(&second_branch),
    );
    fixture.claim(&id, Some(&second), "term-target", &fixture.host);
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");

    let preview = fixture.run(&["gc", &id, "--dry-run"]);
    let issue = gc_issue(&preview, &id);
    gc_status(issue, "worktree", "removed");
    gc_status(issue, "branch", "removed");
    assert!(first.is_dir());
    assert!(second.is_dir());

    let data = fixture.run(&["gc", &id]);
    let issue = gc_issue(&data, &id);
    gc_status(issue, "session", "removed");
    gc_status(issue, "worktree", "removed");
    gc_status(issue, "branch", "removed");
    assert_eq!(issue["session"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(issue["worktree"]["items"].as_array().unwrap().len(), 2);
    assert!(!first.exists());
    assert!(!second.exists());
    assert!(!fixture.branch_exists(&id));
    assert!(fixture.branch_exists(&second_branch));
    let mut closed = fixture.closed_spaces();
    closed.sort();
    assert_eq!(closed, ["history", "target"]);
}

#[test]
fn gc_respects_the_check_queue_lock_including_previews() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    fixture
        .store_at("2026-11-20T10:00:00Z")
        .write("human:alice@example.com", false, |c| {
            c.check_lock_take(i64::from(std::process::id()), Some(&fixture.host))
        })
        .unwrap();
    for args in [
        vec!["gc", id.as_str()],
        vec!["gc", id.as_str(), "--dry-run"],
    ] {
        let output = fixture.cmd().arg("--json").args(args).output().unwrap();
        assert!(!output.status.success());
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["code"], "invalid_state");
        assert!(path.exists());
        assert!(fixture.branch_exists(&id));
        assert!(fixture.closed_spaces().is_empty());
        assert_eq!(fixture.notes(&id), json!([]));
    }
}

#[test]
fn gc_removes_ignored_build_artifacts_through_git() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    fixture.git_at(
        &fixture.e.repo(),
        &[
            "config",
            "core.excludesFile",
            fixture.e.dir.path().join("ignore").to_str().unwrap(),
        ],
    );
    std::fs::write(fixture.e.dir.path().join("ignore"), "target/\n").unwrap();
    std::fs::create_dir(path.join("target")).unwrap();
    std::fs::write(path.join("target/artifact"), "ignored build output").unwrap();
    let preview = fixture.run(&["gc", &id, "--dry-run"]);
    gc_status(gc_issue(&preview, &id), "worktree", "removed");
    assert!(path.join("target/artifact").exists());
    let data = fixture.run(&["gc", &id]);
    gc_status(gc_issue(&data, &id), "worktree", "removed");
    assert!(!path.exists());
}

#[test]
fn gc_rejects_a_worktree_reached_through_an_escaping_symlink() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Outside worktree"]);
    let path = fixture.worktree_at(&id, &fixture.e.dir.path().join("outside"));
    let root = fixture.e.trk_dir().join("worktrees");
    std::fs::create_dir_all(&root).unwrap();
    let link = root.join(&id);
    std::os::unix::fs::symlink(&path, &link).unwrap();
    fixture.claim(&id, Some(&link), "term-target", &fixture.host);
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
    let data = fixture.run(&["gc", &id]);
    gc_status(gc_issue(&data, &id), "worktree", "skipped");
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
}

#[test]
fn gc_still_collects_git_resources_when_the_terminal_is_gone() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "missing-terminal");
    let data = fixture.run(&["gc", &id]);
    let issue = gc_issue(&data, &id);
    gc_status(issue, "session", "absent");
    gc_status(issue, "worktree", "removed");
    gc_status(issue, "branch", "removed");
    assert!(!path.exists());
    assert!(fixture.closed_spaces().is_empty());
}

#[test]
fn gc_without_recorded_worktrees_keeps_an_unrelated_issue_named_branch() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Never launched"]);
    fixture.git_at(&fixture.e.repo(), &["branch", &id]);
    fixture.e.ok(&["close", &id]);
    let notes = fixture.notes(&id);

    for args in [&["gc", &id, "--dry-run"][..], &["gc", &id][..]] {
        let data = fixture.run(args);
        let issue = gc_issue(&data, &id);
        assert_eq!(issue["worktree"]["status"], "absent", "{issue:#}");
        gc_status(issue, "branch", "absent");
        assert!(fixture.branch_exists(&id));
        assert_eq!(fixture.notes(&id), notes);
    }
}

#[test]
fn gc_primary_checkout_claims_do_not_own_git_resources() {
    let fixture = GcEnv::new();
    let id = fixture
        .e
        .create(&["Historical run in the primary checkout"]);
    fixture.claim(
        &id,
        Some(&fixture.e.repo()),
        "missing-terminal",
        &fixture.host,
    );
    fixture.e.ok(&["unclaim", &id]);
    let alias = fixture.e.dir.path().join("primary-alias");
    std::os::unix::fs::symlink(fixture.e.repo(), &alias).unwrap();
    fixture.claim(&id, Some(&alias), "missing-terminal", &fixture.host);
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
    fixture.git_at(&fixture.e.repo(), &["branch", &id]);
    let notes = fixture.notes(&id);
    let claims = fixture.e.ok(&["show", &id, "--with", "claims"])["claims"].clone();

    for args in [&["gc", &id, "--dry-run"][..], &["gc", &id][..]] {
        let data = fixture.run(args);
        let issue = gc_issue(&data, &id);
        assert_eq!(issue["worktree"]["status"], "absent", "{issue:#}");
        gc_status(issue, "branch", "absent");
        assert!(fixture.e.repo().is_dir());
        assert!(fixture.branch_exists(&id));
        assert_eq!(fixture.notes(&id), notes);
        assert_eq!(
            fixture.e.ok(&["show", &id, "--with", "claims"])["claims"],
            claims
        );
    }

    // Each check is a fresh executable, with no in-process warning cache.
    for _ in 0..2 {
        let output = fixture
            .cmd()
            .env("TRK_NOW", "2026-11-20T12:00:00Z")
            .arg("check")
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains("gc:"), "{stdout}");
        assert!(fixture.e.repo().is_dir());
        assert!(fixture.branch_exists(&id));
        assert_eq!(fixture.notes(&id), notes);
        assert_eq!(
            fixture.e.ok(&["show", &id, "--with", "claims"])["claims"],
            claims
        );
    }
}

#[test]
fn gc_absent_historical_worktrees_are_not_outside_directory_skips() {
    for registered in [false, true] {
        let fixture = GcEnv::new();
        let id = fixture.e.create(&["Historical worktree already removed"]);
        fixture.claim(
            &id,
            Some(&fixture.e.repo()),
            "missing-terminal",
            &fixture.host,
        );
        fixture.e.ok(&["unclaim", &id]);
        let path = fixture.worktree_at(
            &id,
            &fixture
                .e
                .dir
                .path()
                .join("Library/Application Support/trk/worktrees")
                .join(&id),
        );
        fixture.claim(&id, Some(&path), "missing-terminal", &fixture.host);
        fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
        if registered {
            std::fs::remove_dir_all(&path).unwrap();
        } else {
            fixture.git_at(
                &fixture.e.repo(),
                &["worktree", "remove", path.to_str().unwrap()],
            );
        }

        let preview = fixture.run(&["gc", &id, "--dry-run"]);
        let issue = gc_issue(&preview, &id);
        gc_status(issue, "worktree", "absent");
        gc_status(
            issue,
            "branch",
            if registered { "skipped" } else { "removed" },
        );
        assert!(fixture.branch_exists(&id));
        if registered {
            assert!(
                issue["branch"]["reason"]
                    .as_str()
                    .unwrap()
                    .contains("retained worktree"),
                "{issue:#}"
            );
        }

        let data = fixture.run(&["gc", &id]);
        let issue = gc_issue(&data, &id);
        gc_status(issue, "worktree", "absent");
        gc_status(
            issue,
            "branch",
            if registered { "skipped" } else { "removed" },
        );
        assert_eq!(fixture.branch_exists(&id), registered);
        assert!(fixture.e.repo().is_dir());

        for _ in 0..2 {
            let data = check_gc_at(&fixture, "2026-11-20T12:00:00Z", &["check"]);
            if registered {
                let issue = gc_issue(&data["gc"], &id);
                gc_status(issue, "worktree", "absent");
                gc_status(issue, "branch", "skipped");
            } else {
                assert!(data.get("gc").is_none(), "{data:#}");
            }
            assert_eq!(fixture.branch_exists(&id), registered);
        }
        if registered {
            let worktrees = fixture.git_at(&fixture.e.repo(), &["worktree", "list", "--porcelain"]);
            assert!(worktrees.contains(path.to_str().unwrap()), "{worktrees}");
        }
    }
}

#[test]
fn gc_worktreeless_run_never_collects_a_coincidentally_named_branch() {
    let fixture = GcEnv::new();
    fixture.type_worktree(false);
    let id = fixture.e.create(&["Run in the project checkout"]);
    fixture
        .cmd()
        .args(["run", &id, "--harness", "omp", "--print"])
        .assert()
        .success();
    let claim = fixture.e.ok(&["show", &id])["claim"].clone();
    assert_eq!(claim["worktree"], Value::Null);
    assert!(!fixture.e.trk_dir().join("worktrees").join(&id).exists());
    assert!(!fixture.branch_exists(&id));

    fixture.git_at(&fixture.e.repo(), &["branch", &id]);
    fixture
        .e
        .ok(&["--as", claim["actor"].as_str().unwrap(), "close", &id]);
    let notes = fixture.notes(&id);
    // Configuration changes cannot turn this launch into ownership of a branch.
    fixture.type_worktree(true);

    for args in [&["gc", &id, "--dry-run"][..], &["gc", &id][..]] {
        let data = fixture.run(args);
        let issue = gc_issue(&data, &id);
        assert_eq!(issue["worktree"]["status"], "absent", "{issue:#}");
        gc_status(issue, "branch", "absent");
        assert!(fixture.branch_exists(&id));
        assert!(fixture.e.repo().is_dir());
        assert_eq!(fixture.notes(&id), notes);
    }

    let sweep = check_gc_at(&fixture, "2026-11-20T12:00:00Z", &["check"]);
    assert!(sweep.get("gc").is_none(), "{sweep:#}");
    assert!(fixture.branch_exists(&id));
    assert!(fixture.e.repo().is_dir());
    assert_eq!(fixture.notes(&id), notes);
}

#[test]
fn gc_collects_historical_worktrees_after_the_type_opts_out() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Previously launched in a worktree"]);
    let path = fixture.worktree(&id);
    fixture.claim(&id, Some(&path), "term-history", &fixture.host);
    fixture.e.ok(&["unclaim", &id]);

    fixture.type_worktree(false);
    fixture
        .cmd()
        .args(["run", &id, "--harness", "omp", "--print"])
        .assert()
        .success();
    let claim = fixture.e.ok(&["show", &id])["claim"].clone();
    assert_eq!(claim["worktree"], Value::Null);
    fixture
        .e
        .ok(&["--as", claim["actor"].as_str().unwrap(), "close", &id]);

    let data = fixture.run(&["gc", &id]);
    let issue = gc_issue(&data, &id);
    for resource in ["session", "worktree", "branch"] {
        gc_status(issue, resource, "removed");
    }
    assert_eq!(issue["worktree"]["items"].as_array().unwrap().len(), 1);
    assert!(!path.exists());
    assert!(!fixture.branch_exists(&id));
    assert!(fixture.e.repo().is_dir());
    assert_eq!(fixture.closed_spaces(), ["history"]);
}

