use super::*;

#[test]
fn gc_legacy_hc_removes_verified_finished_sessions_without_rewriting_claims() {
    for launcher in [None, Some("")] {
        let fixture = GcEnv::new();
        let id = fixture.e.create(&["Historical session"]);
        fixture.legacy_claim(&id, "legacy-address", &fixture.host, launcher);
        fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
        fixture.fake_hc(
            r#"{"terminals":[{"address":"legacy-address"}]}"#,
            &fixture.hc_record(),
        );
        let claims = fixture.e.ok(&["show", &id, "--with", "claims"])["claims"].clone();
        let claim_id: i64 = fixture
            .store_at("2026-11-20T10:00:00Z")
            .conn()
            .query_row("SELECT id FROM claim WHERE issue_id = ?1", [&id], |row| {
                row.get(0)
            })
            .unwrap();

        let data = fixture.run(&["gc", &id, "--legacy-hc"]);
        gc_status(gc_issue(&data, &id), "session", "removed");
        assert_eq!(
            fixture.hc_calls(),
            "ls --all --json\nget legacy-address --json\nrm legacy-address --json\n"
        );
        let receipts = fixture.legacy_receipts(&id);
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0]["claim"], claim_id);
        assert_eq!(receipts[0]["terminal"], "legacy-address");
        assert_eq!(receipts[0]["host"], fixture.host);
        assert_eq!(receipts[0]["backend"], "hc");
        assert_eq!(
            fixture.e.ok(&["show", &id, "--with", "claims"])["claims"],
            claims
        );
        let notes = fixture.notes(&id);

        let data = fixture.run(&["gc", &id]);
        gc_status(gc_issue(&data, &id), "session", "absent");
        assert!(fixture.hc_calls().ends_with("ls --all --json\n"));
        assert_eq!(fixture.hc_calls().matches("rm ").count(), 1);
        assert_eq!(fixture.notes(&id), notes);
        assert_eq!(fixture.legacy_receipts(&id), receipts);
        std::fs::write(fixture.e.dir.path().join("hc-fail-inventory"), "").unwrap();
        let data = fixture.run(&["gc", &id]);
        gc_status(gc_issue(&data, &id), "session", "skipped");
        assert_eq!(fixture.legacy_receipts(&id), receipts);
    }
}

#[test]
fn gc_legacy_hc_missing_exact_address_is_absent_and_preview_writes_no_receipt() {
    for inventory in [
        r#"{"terminals":[]}"#,
        r#"{"terminals":[{"address":"different-address","name":"legacy-address","alias":"legacy-address"}]}"#,
    ] {
        let fixture = GcEnv::new();
        let id = fixture.legacy_closed();
        fixture.fake_hc(inventory, &fixture.hc_record());
        let notes = fixture.notes(&id);

        let preview = fixture.run(&["gc", &id, "--legacy-hc", "--dry-run"]);
        gc_status(gc_issue(&preview, &id), "session", "absent");
        assert_eq!(fixture.notes(&id), notes);
        assert!(fixture.legacy_receipts(&id).is_empty());
        assert_eq!(fixture.hc_calls(), "ls --all --json\n");

        let data = fixture.run(&["gc", &id, "--legacy-hc"]);
        gc_status(gc_issue(&data, &id), "session", "absent");
        assert_eq!(fixture.legacy_receipts(&id).len(), 1);
        assert!(!fixture.hc_calls().contains("get "));
        assert!(!fixture.hc_calls().contains("rm "));
    }
}

#[test]
fn gc_legacy_hc_invalid_inventory_is_skipped_not_absent() {
    for inventory in [
        "not json",
        "{}",
        r#"{"terminals":{}}"#,
        r#"{"terminals":[{}]}"#,
        r#"{"terminals":[{"address":"legacy-address"},{"address":"legacy-address"}]}"#,
        r#"{"terminals":[]}"#,
    ] {
        let fixture = GcEnv::new();
        let id = fixture.legacy_closed();
        fixture.fake_hc(inventory, &fixture.hc_record());
        if inventory == r#"{"terminals":[]}"# {
            std::fs::write(fixture.e.dir.path().join("hc-fail-inventory"), "").unwrap();
        }
        let notes = fixture.notes(&id);
        let data = fixture.run(&["gc", &id, "--legacy-hc"]);
        gc_status(gc_issue(&data, &id), "session", "skipped");
        assert_eq!(fixture.hc_calls(), "ls --all --json\n");
        assert_eq!(fixture.notes(&id), notes);
        assert!(fixture.legacy_receipts(&id).is_empty());
    }
}

#[test]
fn gc_legacy_hc_rejects_unverified_identity_and_live_sessions() {
    for mismatch in ["actor", "actor-substring", "cwd", "state", "address"] {
        let fixture = GcEnv::new();
        let id = fixture.legacy_closed();
        let mut record = fixture.hc_record();
        match mismatch {
            "actor" => record["argv"] = json!(["TRK_ACTOR=someone-else"]),
            "actor-substring" => {
                record["argv"] = json!(["TRK_ACTOR=human:alice@example.com.extra"])
            }
            "cwd" => record["cwd"] = json!(fixture.e.dir.path().join("unrelated")),
            "state" => record["state"] = json!("running"),
            "address" => record["address"] = json!("different-address"),
            _ => unreachable!(),
        }
        fixture.fake_hc(r#"{"terminals":[{"address":"legacy-address"}]}"#, &record);
        let notes = fixture.notes(&id);
        let data = fixture.run(&["gc", &id, "--legacy-hc"]);
        gc_status(gc_issue(&data, &id), "session", "skipped");
        assert!(!fixture.hc_calls().contains("rm "), "{mismatch}");
        assert_eq!(fixture.notes(&id), notes);
        assert!(fixture.legacy_receipts(&id).is_empty());
    }
}

#[test]
fn gc_legacy_hc_preserves_foreign_caller_and_protected_terminals() {
    for gate in ["foreign", "caller", "claimed", "waiting"] {
        let fixture = GcEnv::new();
        let id = fixture.e.create(&["Historical session"]);
        let host = if gate == "foreign" {
            format!("{}.foreign.invalid", fixture.host)
        } else {
            fixture.host.clone()
        };
        fixture.legacy_claim(&id, "legacy-address", &host, None);
        fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
        fixture.fake_hc(
            r#"{"terminals":[{"address":"legacy-address"}]}"#,
            &fixture.hc_record(),
        );
        if matches!(gate, "claimed" | "waiting") {
            let protected = fixture.e.create(&["Protected session"]);
            fixture.legacy_claim(&protected, "legacy-address", &fixture.host, None);
            if gate == "waiting" {
                fixture
                    .e
                    .ok(&["wait", &protected, "--reason", "human decision"]);
            }
        }
        let notes = fixture.notes(&id);
        let mut command = fixture.cmd();
        if gate == "caller" {
            command.env("HC_TERMINAL_ID", "legacy-address");
        }
        let data = gc_success(command, &["gc", &id, "--legacy-hc"]);
        gc_status(gc_issue(&data, &id), "session", "skipped");
        assert!(!fixture.hc_calls().contains("rm "), "{gate}");
        assert_eq!(fixture.notes(&id), notes);
        assert!(fixture.legacy_receipts(&id).is_empty());
    }
}

#[test]
fn gc_legacy_hc_requires_explicit_issue_ids() {
    let fixture = GcEnv::new();
    fixture.legacy_closed();
    fixture.fake_hc(r#"{"terminals":[]}"#, &fixture.hc_record());
    for args in [
        vec!["gc", "--legacy-hc"],
        vec!["gc", "--legacy-hc", "--dry-run"],
    ] {
        let output = fixture.cmd().args(args).output().unwrap();
        assert!(!output.status.success());
    }
    assert!(fixture.hc_calls().is_empty());
}

#[test]
fn gc_legacy_hc_previews_verified_sessions_and_matches_recorded_worktree() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Historical worktree session"]);
    let path = fixture.worktree(&id);
    fixture
        .store_at("2026-11-20T10:00:00Z")
        .write("human:alice@example.com", false, |context| {
            context.claim(
                &id,
                trk_core::ops::ClaimOpts {
                    worktree: Some(path.to_string_lossy().into_owned()),
                    terminal: Some("legacy-address".into()),
                    host: Some(fixture.host.clone()),
                    ..Default::default()
                },
            )?;
            Ok(())
        })
        .unwrap();
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
    let mut record = fixture.hc_record();
    record["cwd"] = json!(path);
    record["argv"] = json!([
        "/bin/sh",
        "-c",
        "env\tTRK_ACTOR=human:alice@example.com trk"
    ]);
    fixture.fake_hc(r#"{"terminals":[{"address":"legacy-address"}]}"#, &record);
    let notes = fixture.notes(&id);
    let claims = fixture.e.ok(&["show", &id, "--with", "claims"])["claims"].clone();

    let preview = fixture.run(&["gc", &id, "--legacy-hc", "--dry-run"]);
    gc_status(gc_issue(&preview, &id), "session", "removed");
    assert!(path.exists());
    assert!(!fixture.hc_calls().contains("rm "));
    assert_eq!(fixture.notes(&id), notes);
    assert!(fixture.legacy_receipts(&id).is_empty());
    assert_eq!(
        fixture.e.ok(&["show", &id, "--with", "claims"])["claims"],
        claims
    );

    let data = fixture.run(&["gc", &id, "--legacy-hc"]);
    gc_status(gc_issue(&data, &id), "session", "removed");
    assert_eq!(fixture.legacy_receipts(&id).len(), 1);
    assert_eq!(
        fixture
            .hc_calls()
            .matches("rm legacy-address --json")
            .count(),
        1
    );
}

#[test]
fn gc_legacy_hc_does_not_override_named_launcher_claims() {
    let fixture = GcEnv::new();
    let (id, _) = fixture.closed("done", "term-target");
    fixture.fake_hc(r#"{"terminals":[]}"#, &fixture.hc_record());

    let data = fixture.run(&["gc", &id, "--legacy-hc"]);
    gc_status(gc_issue(&data, &id), "session", "removed");
    assert_eq!(fixture.closed_spaces(), ["target"]);
    assert!(fixture.hc_calls().is_empty());
    assert!(fixture.legacy_receipts(&id).is_empty());
}

#[test]
fn gc_legacy_hc_failed_identity_probe_or_removal_never_records_success() {
    for failure in ["hc-fail-get", "hc-fail-rm"] {
        let fixture = GcEnv::new();
        let id = fixture.legacy_closed();
        fixture.fake_hc(
            r#"{"terminals":[{"address":"legacy-address"}]}"#,
            &fixture.hc_record(),
        );
        std::fs::write(fixture.e.dir.path().join(failure), "").unwrap();
        let notes = fixture.notes(&id);
        let data = fixture.run(&["gc", &id, "--legacy-hc"]);
        gc_status(gc_issue(&data, &id), "session", "skipped");
        assert!(!fixture.e.dir.path().join("hc-removed").exists());
        assert!(fixture.legacy_receipts(&id).is_empty());
        assert_eq!(fixture.notes(&id), notes);
    }
}

