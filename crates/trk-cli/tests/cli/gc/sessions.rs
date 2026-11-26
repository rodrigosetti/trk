use super::*;

#[test]
fn gc_collects_historical_session_directories_without_terminals_or_worktrees() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Historical session files"]);
    let first = fixture.session_claim(&id, &uuid::Uuid::new_v4().to_string(), &fixture.host);
    fixture.e.ok(&["unclaim", &id, "--force"]);
    let second = fixture.session_claim(&id, &uuid::Uuid::new_v4().to_string(), &fixture.host);
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
    let notes = fixture.notes(&id);
    let preview = fixture.run(&["gc", &id, "--dry-run"]);
    let resource = &gc_issue(&preview, &id)["session_dir"];
    assert_eq!(resource["status"], "removed");
    for item in resource["items"].as_array().unwrap() {
        assert!(
            item["reason"]
                .as_str()
                .unwrap()
                .starts_with("would remove ")
        );
    }
    assert!(first.join("mcp.json").exists());
    assert!(second.join("mcp.json").exists());
    assert_eq!(fixture.notes(&id), notes);

    let data = fixture.run(&["gc", &id]);
    gc_status(gc_issue(&data, &id), "session_dir", "removed");
    let items = gc_issue(&data, &id)["session_dir"]["items"]
        .as_array()
        .unwrap();
    for path in [&first, &second] {
        assert!(
            items
                .iter()
                .any(|item| item["path"] == path.to_str().unwrap())
        );
        assert!(!path.exists());
    }
    let after = fixture.notes(&id);
    assert!(after.as_array().unwrap().iter().any(|note| {
        note["body"]
            .as_str()
            .unwrap()
            .contains("removed session directory")
    }));
    let again = fixture.run(&["gc", &id]);
    gc_status(gc_issue(&again, &id), "session_dir", "absent");
    assert_eq!(fixture.notes(&id), after);
}

#[test]
fn gc_keeps_session_directories_of_open_claimed_waiting_and_foreign_issues() {
    let fixture = GcEnv::new();
    for state in ["open", "claimed", "waiting", "foreign"] {
        let id = fixture.e.create(&["Retain session files"]);
        let host = if state == "foreign" {
            "foreign.invalid"
        } else {
            &fixture.host
        };
        let path = fixture.session_claim(&id, &uuid::Uuid::new_v4().to_string(), host);
        match state {
            "open" => {
                fixture.e.ok(&["unclaim", &id, "--force"]);
            }
            "waiting" => {
                fixture.e.ok(&["wait", &id, "--reason", "human", "--force"]);
            }
            "foreign" => fixture.close_at(&id, "done", "2026-11-20T10:00:00Z"),
            _ => {}
        }
        let data = fixture.run(&["gc", &id, "--abandoned"]);
        assert_eq!(gc_issue(&data, &id)["session_dir"]["status"], "skipped");
        assert_eq!(
            std::fs::read_to_string(path.join("mcp.json")).unwrap(),
            "secret"
        );
    }
}

#[test]
fn gc_session_directories_follow_abandoned_and_age_gates() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Abandoned session files"]);
    let path = fixture.session_claim(&id, &uuid::Uuid::new_v4().to_string(), &fixture.host);
    fixture.close_at(&id, "abandoned", "2026-11-20T10:00:00Z");
    let excluded = fixture.run(&["gc", &id]);
    assert_eq!(gc_issue(&excluded, &id)["session_dir"]["status"], "skipped");
    let young = fixture.run(&["gc", &id, "--abandoned", "--older-than", "36500d"]);
    assert_eq!(gc_issue(&young, &id)["session_dir"]["status"], "skipped");
    assert!(path.exists());
    let included = fixture.run(&["gc", &id, "--abandoned"]);
    gc_status(gc_issue(&included, &id), "session_dir", "removed");
    assert!(!path.exists());
}

#[test]
fn gc_session_directories_reject_escaping_symlinks_and_unsafe_actor_paths() {
    use std::os::unix::fs::symlink;

    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Symlinked session files"]);
    let session = uuid::Uuid::new_v4().to_string();
    let path = fixture.session_claim(&id, &session, &fixture.host);
    let outside = fixture.e.dir.path().join("outside");
    std::fs::rename(&path, &outside).unwrap();
    symlink(&outside, &path).unwrap();
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
    let data = fixture.run(&["gc", &id]);
    gc_status(gc_issue(&data, &id), "session_dir", "skipped");
    assert!(outside.join("mcp.json").exists());
    assert!(path.is_symlink());

    let id = fixture.e.create(&["Unsafe actor path"]);
    fixture
        .store_at("2026-11-20T10:00:00Z")
        .write("agent:omp:../../outside", false, |context| {
            context.claim(
                &id,
                trk_core::ops::ClaimOpts {
                    host: Some(fixture.host.clone()),
                    ..Default::default()
                },
            )?;
            Ok(())
        })
        .unwrap();
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
    let data = fixture.run(&["gc", &id]);
    gc_status(gc_issue(&data, &id), "session_dir", "skipped");
    assert!(outside.join("mcp.json").exists());
}

#[test]
fn gc_session_directories_reject_a_symlinked_sessions_root() {
    use std::os::unix::fs::symlink;

    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Symlinked sessions root"]);
    let path = fixture.session_claim(&id, &uuid::Uuid::new_v4().to_string(), &fixture.host);
    let root = path.parent().unwrap();
    let outside = fixture.e.dir.path().join("outside");
    std::fs::rename(root, &outside).unwrap();
    symlink(&outside, root).unwrap();
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
    let data = fixture.run(&["gc", &id]);
    gc_status(gc_issue(&data, &id), "session_dir", "skipped");
    assert!(path.join("mcp.json").exists());
}

#[test]
fn gc_session_directory_removal_does_not_follow_nested_symlinks() {
    use std::os::unix::fs::symlink;

    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Nested session symlink"]);
    let path = fixture.session_claim(&id, &uuid::Uuid::new_v4().to_string(), &fixture.host);
    let outside = fixture.e.dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep"), "unrelated").unwrap();
    symlink(&outside, path.join("linked")).unwrap();
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
    let data = fixture.run(&["gc", &id]);
    gc_status(gc_issue(&data, &id), "session_dir", "removed");
    assert!(!path.exists());
    assert_eq!(
        std::fs::read_to_string(outside.join("keep")).unwrap(),
        "unrelated"
    );
}

#[test]
fn check_gc_session_directories_preview_collect_and_retry_skipped_items() {
    use std::os::unix::fs::symlink;

    let fixture = GcEnv::new();
    check_gc_config(&fixture, "gc_grace = \"0\"");
    let id = fixture.e.create(&["Automatic session file cleanup"]);
    let first = fixture.session_claim(&id, &uuid::Uuid::new_v4().to_string(), &fixture.host);
    fixture.e.ok(&["unclaim", &id, "--force"]);
    let second = fixture.session_claim(&id, &uuid::Uuid::new_v4().to_string(), &fixture.host);
    let outside = fixture.e.dir.path().join("outside");
    std::fs::rename(&second, &outside).unwrap();
    symlink(&outside, &second).unwrap();
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
    let preview = check_gc_at(&fixture, "2026-11-20T12:00:00Z", &["check", "--dry-run"]);
    gc_status(gc_issue(&preview["gc"], &id), "session_dir", "removed");
    assert!(first.exists());
    assert!(second.is_symlink());

    let first_pass = check_gc_at(&fixture, "2026-11-20T12:00:00Z", &["check"]);
    gc_status(gc_issue(&first_pass["gc"], &id), "session_dir", "removed");
    assert!(!first.exists());
    assert!(outside.join("mcp.json").exists());
    std::fs::remove_file(&second).unwrap();
    std::fs::rename(&outside, &second).unwrap();
    let retry = check_gc_at(&fixture, "2026-11-20T12:00:01Z", &["check"]);
    gc_status(gc_issue(&retry["gc"], &id), "session_dir", "removed");
    assert!(!second.exists());
    let completed = check_gc_at(&fixture, "2026-11-20T12:00:02Z", &["check"]);
    assert!(completed.get("gc").is_none(), "{completed:#}");
}

#[test]
fn gc_keeps_the_current_session_directory() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Current session directory"]);
    let path = fixture.session_claim(&id, &uuid::Uuid::new_v4().to_string(), &fixture.host);
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
    let mut command = fixture.cmd();
    command
        .current_dir(&path)
        .env("TRK_DIR", fixture.e.trk_dir());
    let data = gc_success(command, &["gc", &id]);
    gc_status(gc_issue(&data, &id), "session_dir", "skipped");
    assert!(path.join("mcp.json").exists());
}
