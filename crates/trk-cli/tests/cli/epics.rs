use super::*;

#[test]
fn epic_briefings_show_curated_choices_and_preserve_empty_projects() {
    let e = Env::new();
    let unassigned = e.create(&["Unassigned issue"]);
    let baseline = e.ok(&["agent", "preview", "implementer", "--issue", &unassigned]);
    for (name, description) in [
        ("alpha", "Session safety\nand recovery"),
        ("zebra", "Reliable launches"),
        ("retired", "Old work"),
    ] {
        e.ok(&["epic", "new", name, "-d", description]);
    }
    e.ok(&["epic", "archive", "retired"]);
    let assigned = e.create(&["Assigned issue", "--epic", "alpha"]);
    let preview = e.ok(&["agent", "preview", "implementer", "--issue", &assigned]);
    let briefing = preview["briefing"].as_str().unwrap();
    assert!(briefing.contains("Epic: `alpha`"), "{briefing}");
    assert!(
        briefing.contains("- alpha: Session safety and recovery"),
        "{briefing}"
    );
    assert!(
        briefing.contains("- zebra: Reliable launches"),
        "{briefing}"
    );
    assert!(!briefing.contains("retired"), "{briefing}");
    let sample = e.ok(&["agent", "preview", "implementer"]);
    assert!(
        sample["briefing"]
            .as_str()
            .unwrap()
            .contains("- zebra: Reliable launches")
    );
    let spec = e.ok(&["run", &assigned, "--harness", "omp", "--dry-run"]);
    let launched = spec["briefing"].as_str().unwrap();
    assert!(launched.contains("Epic: `alpha`"), "{launched}");
    assert!(
        launched.contains("- alpha: Session safety and recovery"),
        "{launched}"
    );
    e.ok(&["epic", "archive", "alpha"]);
    e.ok(&["epic", "archive", "zebra"]);
    let archived = e.ok(&["agent", "preview", "implementer", "--issue", &assigned]);
    let briefing = archived["briefing"].as_str().unwrap();
    assert!(briefing.contains("Epic: `alpha`"), "{briefing}");
    assert!(!briefing.contains("- alpha:"), "{briefing}");
    assert!(!briefing.contains("- zebra:"), "{briefing}");
    let empty = e.ok(&["agent", "preview", "implementer", "--issue", &unassigned]);
    assert_eq!(empty["briefing"], baseline["briefing"]);
}

#[test]
fn epic_commands_assign_clear_cascade_archive_and_log() {
    let e = Env::new();
    assert_eq!(e.ok(&["epic", "ls"]), json!([]));
    let created = e.ok(&["epic", "new", "reliability", "-d", "Reliable sessions"]);
    assert_eq!(created["name"], "reliability");
    assert_eq!(created["description"], "Reliable sessions");
    assert_eq!(created["created_by"], "human:alice@example.com");
    assert_eq!(created["archived_at"], Value::Null);
    let a = e.create(&["First member", "--epic", "reliability"]);
    let b = e.create(&["Second member"]);
    assert_eq!(e.ok(&["show", &a])["epic"], "reliability");
    assert_eq!(e.ok(&["show", &b])["epic"], Value::Null);
    assert_eq!(
        e.ok(&["edit", &b, "--epic", "reliability"])["epic"],
        "reliability"
    );
    e.ok(&["close", &b]);
    assert_eq!(e.ok(&["epic", "ls"])[0]["issue_count"], 2);
    let pretty = e
        .cmd()
        .args(["--format", "pretty", "--no-color", "epic", "ls"])
        .output()
        .unwrap();
    assert!(pretty.status.success());
    let pretty = String::from_utf8(pretty.stdout).unwrap();
    assert!(
        pretty.contains("Reliable sessions") && pretty.contains("ISSUES"),
        "{pretty}"
    );

    e.ok(&["epic", "rename", "reliability", "session-safety"]);
    assert_eq!(e.ok(&["show", &a])["epic"], "session-safety");
    assert_eq!(e.ok(&["show", &b])["epic"], "session-safety");
    let pretty = e
        .cmd()
        .args(["--format", "pretty", "--no-color", "show", &a])
        .output()
        .unwrap();
    assert!(pretty.status.success());
    let pretty = String::from_utf8(pretty.stdout).unwrap();
    assert!(pretty.contains("epic session-safety"), "{pretty}");
    e.ok(&["epic", "archive", "session-safety"]);
    assert_eq!(e.ok(&["epic", "ls"]), json!([]));
    let all = e.ok(&["epic", "ls", "--all"]);
    assert_eq!(all[0]["name"], "session-safety");
    assert_eq!(all[0]["issue_count"], 2);
    assert!(all[0]["archived_at"].is_string());
    assert_eq!(
        e.ok(&["edit", &a, "-t", "Retitled"])["epic"],
        "session-safety"
    );
    assert_eq!(e.ok(&["edit", &a, "--clear-epic"])["epic"], Value::Null);
    assert_eq!(e.ok(&["epic", "ls", "--all"])[0]["issue_count"], 1);

    let events = e.ok(&["log"]);
    let epic_events: Vec<_> = events
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["op"].as_str().unwrap().starts_with("epic_"))
        .collect();
    assert_eq!(
        epic_events
            .iter()
            .map(|v| v["op"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["epic_create", "epic_rename", "epic_archive"]
    );
    assert!(epic_events.iter().all(|v| v["issue_id"].is_null()));
    assert_eq!(
        epic_events[1]["payload"],
        json!({"from": "reliability", "to": "session-safety"})
    );
    let membership: Vec<_> = e
        .ok(&["log", &a])
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["payload"]["changes"].get("epic").cloned())
        .collect();
    assert_eq!(
        membership,
        [
            json!({"from": null, "to": "reliability"}),
            json!({"from": "session-safety", "to": null})
        ]
    );
    let pretty = e
        .cmd()
        .args(["--format", "pretty", "--no-color", "log"])
        .output()
        .unwrap();
    assert!(pretty.status.success());
    let pretty = String::from_utf8(pretty.stdout).unwrap();
    assert!(pretty.contains("created epic reliability"), "{pretty}");
    assert!(
        pretty.contains("renamed epic reliability → session-safety"),
        "{pretty}"
    );
    assert!(pretty.contains("archived epic session-safety"), "{pretty}");
}

#[test]
fn epic_assignment_errors_list_only_active_choices_and_never_create() {
    let e = Env::new();
    let error = e.err(
        &["create", "No implicit epic", "--epic", "unknown"],
        4,
        "invalid_field",
    );
    assert_eq!(error["allowed"], json!([]));
    assert_eq!(e.ok(&["epic", "ls", "--all"]), json!([]));
    for name in ["zebra", "archived", "alpha"] {
        e.ok(&["epic", "new", name]);
    }
    e.ok(&["epic", "archive", "archived"]);
    let id = e.create(&["Unassigned"]);
    for invalid in ["unknown", "archived"] {
        let error = e.err(
            &["create", "Rejected", "--epic", invalid],
            4,
            "invalid_field",
        );
        assert_eq!(error["allowed"], json!(["alpha", "zebra"]));
        assert!(error["hint"].as_str().unwrap().contains("omit"));
        let error = e.err(&["edit", &id, "--epic", invalid], 4, "invalid_field");
        assert_eq!(error["allowed"], json!(["alpha", "zebra"]));
    }
    assert_eq!(e.ok(&["show", &id])["epic"], Value::Null);
    assert_eq!(e.ok(&["list"])[0]["id"], id);
    assert_eq!(e.ok(&["list"]).as_array().unwrap().len(), 1);
    e.ok(&[
        "epic",
        "new",
        "agent-curated",
        "--as",
        "agent:omp:epic-curator",
    ]);
    assert_eq!(
        e.ok(&["epic", "ls"])[0]["created_by"],
        "agent:omp:epic-curator"
    );
}
