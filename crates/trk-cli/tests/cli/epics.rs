use super::*;

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

#[test]
fn list_and_ready_filter_epics_including_unassigned_and_archived() {
    let e = Env::new();
    for name in ["auth-rework", "billing", "none", "null"] {
        e.ok(&["epic", "new", name]);
    }
    let auth = e.create(&["Auth", "--epic", "auth-rework"]);
    let billing = e.create(&["Billing", "--epic", "billing"]);
    let none = e.create(&["Literal none", "--epic", "none"]);
    let null = e.create(&["Literal null", "--epic", "null"]);
    let unassigned = e.create(&["Unassigned"]);
    e.ok(&["epic", "archive", "auth-rework"]);
    let ids = |rows: Value| {
        rows.as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    for command in ["list", "ready"] {
        for (query, expected) in [
            ("epic=auth-rework", vec![auth.clone()]),
            ("epic=auth-*", vec![auth.clone()]),
            ("epic~billing", vec![billing.clone()]),
            ("epic=\"\"", vec![unassigned.clone()]),
            ("epic=none", vec![none.clone()]),
            ("epic=null", vec![null.clone()]),
            (
                "epic in (auth-rework,\"\")",
                vec![auth.clone(), unassigned.clone()],
            ),
            (
                "epic!=\"\"",
                vec![auth.clone(), billing.clone(), none.clone(), null.clone()],
            ),
            (
                "NOT epic=\"\"",
                vec![auth.clone(), billing.clone(), none.clone(), null.clone()],
            ),
        ] {
            let mut actual = ids(e.ok(&[command, query]));
            let mut expected = expected;
            actual.sort();
            expected.sort();
            assert_eq!(actual, expected, "{command} {query}");
        }
        let rows = e.ok(&[command, "--sort", "id"]);
        let rows = rows.as_array().unwrap();
        for (id, epic) in [(&auth, json!("auth-rework")), (&unassigned, Value::Null)] {
            let row = rows.iter().find(|row| row["id"] == *id).unwrap();
            assert_eq!(row.get("epic"), Some(&epic));
        }
        assert_eq!(
            e.ok(&[command, "epic=auth-rework", "--fields", "id,epic"]),
            json!([{"id": auth, "epic": "auth-rework"}])
        );
    }
}

#[test]
fn list_and_ready_sort_epics_with_unassigned_last_in_both_directions() {
    let e = Env::new();
    e.ok(&["epic", "new", "auth-rework"]);
    e.ok(&["epic", "new", "billing"]);
    let auth_p1 = e.create(&["Auth P1", "--epic", "auth-rework", "-p", "P1"]);
    let auth_p2 = e.create(&["Auth P2", "--epic", "auth-rework", "-p", "P2"]);
    let billing = e.create(&["Billing", "--epic", "billing", "-p", "P3"]);
    let unassigned = e.create(&["Unassigned", "-p", "P0"]);
    let mut auth_ids = vec![auth_p1.clone(), auth_p2.clone()];
    auth_ids.sort();
    let ids = |rows: Value| {
        rows.as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    for command in ["list", "ready"] {
        let mut ascending = auth_ids.clone();
        ascending.extend([billing.clone(), unassigned.clone()]);
        assert_eq!(ids(e.ok(&[command, "--sort", "epic"])), ascending);
        let mut descending = vec![billing.clone()];
        descending.extend(auth_ids.clone());
        descending.push(unassigned.clone());
        assert_eq!(ids(e.ok(&[command, "--sort", "-epic"])), descending);
        assert_eq!(
            ids(e.ok(&[command, "--sort", "epic,-priority"])),
            vec![
                auth_p2.clone(),
                auth_p1.clone(),
                billing.clone(),
                unassigned.clone()
            ]
        );
    }
}
